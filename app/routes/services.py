"""Services route — GET /services, POST /services/{name}/restart|start|stop"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import HTMLResponse
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


def _mgr(request: Request):
    # ServiceManager is now in the Rust binary; Python mgr is no longer available.
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
        # Python ServiceManager is gone — discover from service.json manifests and
        # overlay running status from the Rust API /tools endpoint.
        go_services, rust_services = _discover_services(request.app.state.root)
        import httpx
        api_client = getattr(request.app.state, "api_client", None)
        running_svcs: set[str] = set()
        if api_client is not None:
            try:
                resp = await api_client.get("/tools", timeout=3.0)
                for t in resp.json().get("tools", []):
                    running_svcs.add(t.get("service", ""))
            except Exception:
                pass
        for s in go_services + rust_services:
            if s["name"] in running_svcs:
                s["status"] = "running"
        go_services = [_merge_state(s) for s in go_services]
        rust_services = [_merge_state(s) for s in rust_services]

    return templates.TemplateResponse(request, "services.html", {
        "request": request,
        "active": "services",
        "services": go_services,
        "rust_services": rust_services,
        "mgr_status": mgr is not None,
    })


# ---------------------------------------------------------------------------
# Service control endpoints (all return HTMX HTML snippets)
# ---------------------------------------------------------------------------

async def _api_tool(request: Request, tool: str, name: str) -> bool:
    """Call a service management tool on the Rust API. Returns True on success."""
    import httpx
    api_client = getattr(request.app.state, "api_client", None)
    if api_client is None:
        return False
    try:
        resp = await api_client.post(f"/tool/{tool}", content=f'{{"name":"{name}"}}',
                                     headers={"Content-Type": "application/json"}, timeout=10.0)
        return resp.is_success
    except Exception:
        return False


@router.post("/services/{name}/restart", response_class=HTMLResponse)
async def restart_service(name: str, request: Request):
    """Signal the Rust openagent binary to restart a service."""
    store = _store(request)
    ok = await _api_tool(request, "service.restart", name)
    if ok:
        return HTMLResponse(
            f'<span class="text-[#FF9933] text-sm">Restarting <strong>{name}</strong>…</span>'
        )
    return HTMLResponse(
        f'<span class="text-red-400 text-sm">Could not restart <strong>{name}</strong> — check Rust binary.</span>'
    )


@router.post("/services/{name}/stop", response_class=HTMLResponse)
async def stop_service(name: str, request: Request):
    """Persist disabled state and signal the Rust binary to stop the service."""
    store = _store(request)
    if store:
        await store.set_service_enabled(name, False)

    ok = await _api_tool(request, "service.stop", name)
    if ok:
        return HTMLResponse(
            f'<span class="text-stone-400 text-sm"><strong>{name}</strong> stopped.</span>'
        )
    return HTMLResponse(
        f'<span class="text-red-400 text-sm">Could not stop <strong>{name}</strong> — check Rust binary.</span>'
    )


@router.post("/services/{name}/start", response_class=HTMLResponse)
async def start_service(name: str, request: Request):
    """Persist enabled state and signal the Rust binary to start the service."""
    store = _store(request)
    if store:
        await store.set_service_enabled(name, True)

    ok = await _api_tool(request, "service.start", name)
    if ok:
        return HTMLResponse(
            f'<span class="text-sage text-sm">Starting <strong>{name}</strong>…</span>'
        )
    return HTMLResponse(
        f'<span class="text-red-400 text-sm">Could not start <strong>{name}</strong> — check Rust binary.</span>'
    )
