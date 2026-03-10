"""Services route — GET /services, POST /services/{name}/restart|start|stop"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

from openagent.services.manager import ServiceManager

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


def _mgr(request: Request) -> ServiceManager | None:
    return getattr(request.app.state, "service_manager", None)


def _store(request: Request):
    return getattr(request.app.state, "settings_store", None)


def _discover_services(root: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Scan ``root/services/*/service.json`` and split into Go and Rust service lists."""
    services_dir = root / "services"
    go_services: list[dict[str, Any]] = []
    rust_services: list[dict[str, Any]] = []

    if not services_dir.exists():
        return go_services, rust_services

    for svc_dir in sorted(services_dir.iterdir()):
        manifest_path = svc_dir / "service.json"
        if not manifest_path.exists():
            continue
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            entry = {
                "name": manifest.get("name", svc_dir.name),
                "description": manifest.get("description", ""),
                "version": manifest.get("version", ""),
                "status": "stopped",
                "tools": manifest.get("tools", []),
                "events": manifest.get("events", []),
                "restart_count": 0,
                "last_error": None,
                "socket": manifest.get("socket", ""),
                "runtime": manifest.get("runtime", "go"),
                "enabled": True,
            }
        except (json.JSONDecodeError, OSError):
            entry = {
                "name": svc_dir.name,
                "description": "",
                "version": "",
                "status": "error",
                "tools": [],
                "events": [],
                "restart_count": 0,
                "last_error": "invalid service.json",
                "socket": "",
                "runtime": "go",
                "enabled": True,
            }

        if entry["runtime"] == "rust":
            rust_services.append(entry)
        else:
            go_services.append(entry)

    return go_services, rust_services


@router.get("/services")
async def services_page(request: Request):
    mgr = _mgr(request)
    store = _store(request)

    # Load persisted enabled states from both sources
    svc_states: dict[str, dict] = {}
    if store:
        svc_states = await store.get_all_service_states()
        # Also fold in connector.*.enabled=0 from the settings key-value store
        connector_settings = await store.get_all(prefix="connector.")
        for key, val in connector_settings.items():
            if key.endswith(".enabled"):
                name = key.split(".")[1]
                if name not in svc_states:
                    svc_states[name] = {"enabled": val != "0", "last_started": None,
                                        "last_stopped": None, "last_error": None,
                                        "restart_count": 0}
                elif val == "0":
                    svc_states[name]["enabled"] = False

    def _merge_state(svc_dict: dict) -> dict:
        name = svc_dict["name"]
        if name in svc_states:
            st = svc_states[name]
            svc_dict["enabled"] = st["enabled"]
            svc_dict.setdefault("last_started", st.get("last_started"))
            svc_dict.setdefault("last_stopped", st.get("last_stopped"))
            # only overwrite last_error from DB if manager has none
            if not svc_dict.get("last_error"):
                svc_dict["last_error"] = st.get("last_error")
        else:
            svc_dict.setdefault("enabled", True)
        return svc_dict

    if mgr:
        all_svcs = [_merge_state(s.to_dict()) for s in mgr.list_services()]
        go_services = [s for s in all_svcs if s.get("runtime", "go") != "rust"]
        rust_services = [s for s in all_svcs if s.get("runtime", "go") == "rust"]
    else:
        go_services, rust_services = _discover_services(request.app.state.root)
        go_services = [_merge_state(s) for s in go_services]
        rust_services = [_merge_state(s) for s in rust_services]

    return templates.TemplateResponse("services.html", {
        "request": request,
        "active": "services",
        "services": go_services,
        "rust_services": rust_services,
        "mgr_status": mgr is not None,
    })


# ---------------------------------------------------------------------------
# Service control endpoints (all return HTMX HTML snippets)
# ---------------------------------------------------------------------------

@router.post("/services/{name}/restart", response_class=HTMLResponse)
async def restart_service(name: str, request: Request):
    """Terminate service process; watchdog will relaunch with back-off."""
    mgr = _mgr(request)
    if mgr is None:
        return HTMLResponse('<span class="text-red-400 text-sm">ServiceManager not available.</span>')

    matches = [s for s in mgr.list_services() if s.name == name]
    if not matches:
        return HTMLResponse(
            f'<span class="text-red-400 text-sm">Service <strong>{name}</strong> not found.</span>'
        )

    svc = matches[0]
    if svc._process and svc._process.returncode is None:
        svc._process.terminate()
        return HTMLResponse(
            f'<span class="text-[#FF9933] text-sm">Restarting <strong>{name}</strong>…</span>'
        )
    return HTMLResponse(
        f'<span class="text-stone-400 text-sm"><strong>{name}</strong> is not running.</span>'
    )


@router.post("/services/{name}/stop", response_class=HTMLResponse)
async def stop_service(name: str, request: Request):
    """Permanently stop a service and persist the disabled state to SQLite."""
    mgr = _mgr(request)
    store = _store(request)

    if mgr is None:
        return HTMLResponse('<span class="text-red-400 text-sm">ServiceManager not available.</span>')

    # Persist intent before stopping so a crash-restart doesn't re-enable it
    if store:
        await store.set_service_enabled(name, False)

    ok = await mgr.stop_service(name)
    if ok:
        return HTMLResponse(
            f'<span class="text-stone-400 text-sm"><strong>{name}</strong> stopped.</span>'
        )
    return HTMLResponse(
        f'<span class="text-red-400 text-sm">Service <strong>{name}</strong> not found.</span>'
    )


@router.post("/services/{name}/start", response_class=HTMLResponse)
async def start_service(name: str, request: Request):
    """Re-enable and start a previously stopped service."""
    mgr = _mgr(request)
    store = _store(request)

    if mgr is None:
        return HTMLResponse('<span class="text-red-400 text-sm">ServiceManager not available.</span>')

    # Persist enabled intent before starting
    if store:
        await store.set_service_enabled(name, True)

    ok = await mgr.reload(name)
    if ok:
        return HTMLResponse(
            f'<span class="text-sage text-sm">Starting <strong>{name}</strong>…</span>'
        )
    return HTMLResponse(
        f'<span class="text-red-400 text-sm">Failed to start <strong>{name}</strong>. Check binary.</span>'
    )
