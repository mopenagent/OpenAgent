"""Dashboard route — GET /"""

from __future__ import annotations

import importlib.metadata
import json
import time
from pathlib import Path

import psutil
from fastapi import APIRouter, Request
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py

_START_TIME = time.time()


def _system_stats() -> dict:
    cpu = psutil.cpu_percent(interval=0.1)
    ram = psutil.virtual_memory()
    disk = psutil.disk_usage("/")
    temp: float | None = None
    try:
        temps = psutil.sensors_temperatures()
        if temps:
            for entries in temps.values():
                if entries:
                    temp = entries[0].current
                    break
    except AttributeError:
        pass  # Windows / platforms without sensor support

    uptime_s = int(time.time() - _START_TIME)
    h, rem = divmod(uptime_s, 3600)
    m, s = divmod(rem, 60)

    return {
        "cpu_pct": cpu,
        "ram_pct": ram.percent,
        "ram_used_mb": ram.used // (1024 * 1024),
        "ram_total_mb": ram.total // (1024 * 1024),
        "disk_pct": disk.percent,
        "disk_used_gb": disk.used // (1024 ** 3),
        "disk_total_gb": disk.total // (1024 ** 3),
        "temp_c": round(temp, 1) if temp is not None else None,
        "uptime": f"{h:02d}:{m:02d}:{s:02d}",
    }


def _installed_extensions() -> list[dict]:
    eps = importlib.metadata.entry_points(group="openagent.extensions")
    result = []
    for ep in eps:
        try:
            dist = importlib.metadata.distribution(ep.value.split(":")[0].split(".")[0])
            version = dist.metadata["Version"]
        except Exception:
            version = "?"
        result.append({"name": ep.name, "entry": ep.value, "version": version, "status": "installed"})
    return result


def _discover_services(root: Path) -> list[dict]:
    services_dir = root / "services"
    result = []
    if services_dir.exists():
        for manifest in sorted(services_dir.glob("*/service.json")):
            try:
                data = json.loads(manifest.read_text())
                result.append({
                    "name": data.get("name", manifest.parent.name),
                    "description": data.get("description", ""),
                    "version": data.get("version", "?"),
                    "status": "stopped",  # ServiceManager not built yet
                })
            except Exception:
                pass
    return result


@router.get("/")
async def dashboard(request: Request):
    return templates.TemplateResponse("dashboard.html", {
        "request": request,
        "active": "dashboard",
        "stats": _system_stats(),
        "extensions": _installed_extensions(),
        "services": _discover_services(request.app.state.root),
    })


@router.get("/api/stats")
async def stats_partial(request: Request):
    """Partial for HTMX stat-card polling — returns cards only, no layout."""
    return templates.TemplateResponse("_stats_cards.html", {
        "request": request,
        "stats": _system_stats(),
        "extensions": _installed_extensions(),
        "services": _discover_services(request.app.state.root),
    })
