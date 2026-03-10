"""Dashboard route — GET /"""

from __future__ import annotations

import time
import tomllib
from pathlib import Path
from typing import Any

import psutil
from fastapi import APIRouter, Request
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py

_START_TIME = time.time()


def _online_services_from_manager(mgr) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Get Go and Rust services from ServiceManager, filtered to online (running) only."""
    go_services: list[dict[str, Any]] = []
    rust_services: list[dict[str, Any]] = []

    for svc in mgr.list_services():
        d = svc.to_dict()
        if d.get("status") != "running":
            continue
        entry = {
            "name": d["name"],
            "version": d.get("version", "?"),
            "status": "online",
            "memory_mb": d.get("memory_mb"),
        }
        if d.get("runtime") == "rust":
            rust_services.append(entry)
        else:
            go_services.append(entry)

    return go_services, rust_services


def _get_vram_gb() -> tuple[float | None, float | None]:
    import sys
    import subprocess
    import re
    if sys.platform == "darwin":
        try:
            out = subprocess.run(["ioreg", "-l"], capture_output=True, timeout=1).stdout
            m = re.search(br'"Alloc system memory"=(\d+)', out)
            if m:
                used_gb = int(m.group(1)) / (1024**3)
                total_gb = psutil.virtual_memory().total / (1024**3)
                return round(used_gb, 1), round(total_gb, 1)
        except Exception:
            pass
    return None, None

def _system_stats() -> dict:
    cpu = psutil.cpu_percent(interval=0.1)
    ram = psutil.virtual_memory()
    vram_used, vram_total = _get_vram_gb()
    
    # Calculate percentage if we have vram stats, else None
    vram_pct = None
    if vram_used is not None and vram_total and vram_total > 0:
        vram_pct = round((vram_used / vram_total) * 100, 1)

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
        "vram_pct": vram_pct,
        "vram_used_gb": vram_used,
        "vram_total_gb": vram_total,
        "temp_c": round(temp, 1) if temp is not None else None,
        "uptime": f"{h:02d}:{m:02d}:{s:02d}",
    }



def _python_packages(root: Path) -> list[dict[str, Any]]:
    """Build Python packages list: OpenAgent (core) first, then extensions from extensions/."""
    result: list[dict[str, Any]] = []

    # Current process memory (RSS) in MB — shared by core + all extensions
    try:
        proc = psutil.Process()
        memory_mb = round(proc.memory_info().rss / (1024 * 1024), 1)
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        memory_mb = None

    # OpenAgent core — always first
    try:
        import importlib.metadata
        core_version = importlib.metadata.version("openagent-core")
    except Exception:
        core_version = "?"
    result.append({
        "name": "OpenAgent",
        "package": "openagent-core",
        "version": core_version,
        "status": "installed",
        "memory_mb": memory_mb,
        "memory_display": f"{memory_mb} MB" if memory_mb is not None else "—",
    })

    return result


def _get_latest_log_content(root: Path, max_lines: int = 1000) -> str:
    """Read the last N lines from the latest log file in the logs/ directory."""
    logs_dir = root / "logs"
    if not logs_dir.exists() or not logs_dir.is_dir():
        return ""
    
    # Find the most recently modified .log file
    log_files = list(logs_dir.glob("*.log"))
    if not log_files:
        return ""
    
    latest_log_file = max(log_files, key=lambda f: f.stat().st_mtime)
    
    try:
        # Read the file and return the last N lines
        with open(latest_log_file, "r", encoding="utf-8") as f:
            lines = f.readlines()
            return "".join(lines[-max_lines:])
    except Exception:
        return ""


@router.get("/")
async def dashboard(request: Request):
    import asyncio
    root = getattr(request.app.state, "root", Path.cwd())
    mgr = getattr(request.app.state, "service_manager", None)
    if mgr:
        services, rust_services = _online_services_from_manager(mgr)
    else:
        services = []
        rust_services = []

    # Run blocking psutil + disk I/O in a thread so the event loop stays free
    stats, packages = await asyncio.gather(
        asyncio.to_thread(_system_stats),
        asyncio.to_thread(_python_packages, root),
    )

    return templates.TemplateResponse("dashboard.html", {
        "request": request,
        "active": "dashboard",
        "stats": stats,
        "python_packages": packages,
        "services": services,
        "rust_services": rust_services,
    })


@router.get("/api/stats")
async def stats_partial(request: Request):
    """Partial for HTMX stat-card polling — returns cards only, no layout."""
    import asyncio
    root = getattr(request.app.state, "root", Path.cwd())
    mgr = getattr(request.app.state, "service_manager", None)
    if mgr:
        services, rust_services = _online_services_from_manager(mgr)
    else:
        services = []
        rust_services = []

    # Run blocking psutil + disk I/O in a thread so the event loop stays free
    stats, packages = await asyncio.gather(
        asyncio.to_thread(_system_stats),
        asyncio.to_thread(_python_packages, root),
    )

    return templates.TemplateResponse("_stats_cards.html", {
        "request": request,
        "stats": stats,
        "python_packages": packages,
        "services": services,
        "rust_services": rust_services,
    })
