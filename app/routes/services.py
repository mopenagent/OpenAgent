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


def _get_services(root: Path) -> list[dict[str, Any]]:
    """Scan ``root/services/*/service.json`` and return a summary list.

    Each entry has at minimum ``name`` and ``status`` ("stopped" or "error").
    Used for static discovery independent of a running ServiceManager.
    """
    services_dir = root / "services"
    if not services_dir.exists():
        return []
    result: list[dict[str, Any]] = []
    for svc_dir in sorted(services_dir.iterdir()):
        manifest_path = svc_dir / "service.json"
        if not manifest_path.exists():
            continue
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
            result.append({
                "name": manifest.get("name", svc_dir.name),
                "description": manifest.get("description", ""),
                "version": manifest.get("version", ""),
                "status": "stopped",
                "tools": manifest.get("tools", []),
                "events": manifest.get("events", []),
                "restart_count": 0,
                "last_error": None,
                "socket": manifest.get("socket", ""),
            })
        except (json.JSONDecodeError, OSError):
            result.append({
                "name": svc_dir.name,
                "description": "",
                "version": "",
                "status": "error",
                "tools": [],
                "events": [],
                "restart_count": 0,
                "last_error": "invalid service.json",
                "socket": "",
            })
    return result


@router.get("/services")
async def services_page(request: Request):
    mgr: ServiceManager | None = getattr(request.app.state, "service_manager", None)
    service_list = [s.to_dict() for s in mgr.list_services()] if mgr else []
    return templates.TemplateResponse("services.html", {
        "request": request,
        "active": "services",
        "services": service_list,
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
