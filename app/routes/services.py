"""Services route — GET /services, POST /services/{name}/restart"""

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


def _discover_services(root: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Scan ``root/services/*/service.json`` and split into Go and Rust service lists.

    Uses the ``runtime`` field in each manifest to classify (``"rust"`` or ``"go"``).
    Defaults to ``"go"`` if not present. Returns (go_services, rust_services).
    """
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
            }

        if entry["runtime"] == "rust":
            rust_services.append(entry)
        else:
            go_services.append(entry)

    return go_services, rust_services


@router.get("/services")
async def services_page(request: Request):
    mgr: ServiceManager | None = getattr(request.app.state, "service_manager", None)

    if mgr:
        # ServiceManager is live — use its in-memory state, then split by runtime
        all_svcs = [s.to_dict() for s in mgr.list_services()]
        go_services = [s for s in all_svcs if s.get("runtime", "go") != "rust"]
        rust_services = [s for s in all_svcs if s.get("runtime", "go") == "rust"]
    else:
        # Static directory scan — already split
        go_services, rust_services = _discover_services(request.app.state.root)

    return templates.TemplateResponse("services.html", {
        "request": request,
        "active": "services",
        "services": go_services,
        "rust_services": rust_services,
        "mgr_status": mgr is not None
    })


@router.post("/services/{name}/restart", response_class=HTMLResponse)
async def restart_service(name: str, request: Request):
    """Terminate service process; watchdog will relaunch with back-off."""
    mgr: ServiceManager | None = getattr(request.app.state, "service_manager", None)
    if mgr is None:
        return HTMLResponse(
            '<span class="text-red-400 text-sm">ServiceManager not available.</span>'
        )

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
