"""Dashboard route — GET /"""

from __future__ import annotations

import time
from pathlib import Path

import psutil
from fastapi import APIRouter, Request
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py

_START_TIME = time.time()



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



def _python_packages(root: Path) -> list[dict]:
    """Web UI process memory and version."""
    try:
        proc = psutil.Process()
        memory_mb = round(proc.memory_info().rss / (1024 * 1024), 1)
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        memory_mb = None

    try:
        import importlib.metadata
        version = importlib.metadata.version("openagent-app")
    except Exception:
        version = "?"

    return [{
        "name": "App",
        "package": "openagent-app",
        "version": version,
        "status": "running",
        "memory_mb": memory_mb,
        "memory_display": f"{memory_mb} MB" if memory_mb is not None else "—",
    }]


def _get_latest_log_content(root: Path, max_lines: int = 1000) -> str:
    """Read the last N lines from the latest log file in the logs/ directory."""
    logs_dir = root / "logs"
    if not logs_dir.exists() or not logs_dir.is_dir():
        return ""

    log_files = [
        f for f in logs_dir.iterdir()
        if f.is_file()
        and (
            f.suffix in {".log", ".jsonl"}
            or ".logs." in f.name
        )
    ]
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


def _read_manifests(root: Path) -> dict[str, dict]:
    """Read all service.json manifests. Returns {name: manifest_dict}."""
    manifests: dict[str, dict] = {}
    services_dir = root / "services"
    if not services_dir.exists():
        return manifests
    import json
    for svc_dir in services_dir.iterdir():
        manifest_path = svc_dir / "service.json"
        if not manifest_path.exists():
            continue
        try:
            m = json.loads(manifest_path.read_text())
            manifests[m["name"]] = m
        except Exception:
            pass
    return manifests



async def _all_services(request: Request) -> tuple[list[dict], list[dict]]:
    """Return (go_services, rust_services) for the dashboard.

    Sources:
      - service.json manifests  → full list + runtime classification + version
      - Rust /health            → openagent RSS, uptime, tool_count, per-service RSS + status
    """
    import asyncio

    root: Path = getattr(request.app.state, "root", Path.cwd())
    api_client = getattr(request.app.state, "api_client", None)

    manifests_fut = asyncio.to_thread(_read_manifests, root)

    async def _health():
        if api_client is None:
            return {}
        try:
            r = await api_client.get("/health", timeout=5.0)
            return r.json() if r.is_success else {}
        except Exception:
            return {}

    manifests, health_data = await asyncio.gather(manifests_fut, _health())

    runtime_ok = health_data.get("status") == "ok"

    # Build name → rss_mb from health_data["services"] list
    svc_rss: dict[str, float | None] = {}
    svc_running: set[str] = set()
    for svc in health_data.get("services", []):
        name = svc.get("name", "")
        svc_rss[name] = svc.get("rss_mb")
        if svc.get("status") == "running":
            svc_running.add(name)

    # openagent runtime self-info
    self_info = health_data.get("self", {})
    self_rss = self_info.get("rss_mb")

    def _fmt(mb: float | None) -> str:
        return f"{mb} MB" if mb is not None else "—"

    runtime_entry = {
        "name": "openagent",
        "version": "?",
        "status": "online" if runtime_ok else "offline",
        "memory_mb": self_rss,
        "memory_display": _fmt(self_rss),
        "tool_count": health_data.get("tool_count", 0),
        "uptime_s": health_data.get("uptime_s"),
    }

    go_services: list[dict] = []
    rust_services: list[dict] = [runtime_entry]

    for name, manifest in sorted(manifests.items()):
        rss = svc_rss.get(name)
        running = name in svc_running
        entry = {
            "name": name,
            "version": manifest.get("version", "?"),
            "status": "online" if running else "stopped",
            "memory_mb": rss,
            "memory_display": _fmt(rss),
        }
        if manifest.get("runtime", "go") == "rust":
            rust_services.append(entry)
        else:
            go_services.append(entry)

    return go_services, rust_services


@router.get("/")
async def dashboard(request: Request):
    import asyncio
    root = getattr(request.app.state, "root", Path.cwd())

    (stats, packages), (go_services, rust_services) = await asyncio.gather(
        asyncio.gather(
            asyncio.to_thread(_system_stats),
            asyncio.to_thread(_python_packages, root),
        ),
        _all_services(request),
    )

    return templates.TemplateResponse(request, "dashboard.html", {
        "request": request,
        "active": "dashboard",
        "stats": stats,
        "python_packages": packages,
        "services": go_services,
        "rust_services": rust_services,
    })


@router.get("/api/stats")
async def stats_partial(request: Request):
    """Partial for HTMX stat-card polling — returns cards only, no layout."""
    import asyncio
    root = getattr(request.app.state, "root", Path.cwd())

    (stats, packages), (go_services, rust_services) = await asyncio.gather(
        asyncio.gather(
            asyncio.to_thread(_system_stats),
            asyncio.to_thread(_python_packages, root),
        ),
        _all_services(request),
    )

    return templates.TemplateResponse(request, "_stats_cards.html", {
        "request": request,
        "stats": stats,
        "python_packages": packages,
        "services": go_services,
        "rust_services": rust_services,
    })
