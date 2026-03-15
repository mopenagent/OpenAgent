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


def _process_memory_map() -> dict[str, float]:
    """Return {process_name: rss_mb} for all running processes."""
    mem: dict[str, float] = {}
    try:
        for proc in psutil.process_iter(["name", "memory_info"]):
            try:
                name = proc.info["name"] or ""
                rss = proc.info["memory_info"].rss if proc.info["memory_info"] else 0
                # Strip platform suffixes like -darwin-arm64, -linux-arm64
                base = name.split("-")[0]
                mb = round(rss / (1024 * 1024), 1)
                # Keep highest RSS if multiple procs match same base name
                if base not in mem or mb > mem[base]:
                    mem[base] = mb
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass
    except Exception:
        pass
    return mem


async def _all_services(request: Request) -> tuple[list[dict], list[dict]]:
    """Return (go_services, rust_services) for the dashboard.

    Sources:
      - service.json manifests  → full list + runtime classification
      - Rust /health            → openagent runtime status + tool_count
      - Rust /tools             → which child services are registered (running)
      - psutil process scan     → per-process RSS memory
    """
    import asyncio

    root: Path = getattr(request.app.state, "root", Path.cwd())
    api_client = getattr(request.app.state, "api_client", None)

    # Run blocking I/O in threads alongside async API calls
    manifests_fut = asyncio.to_thread(_read_manifests, root)
    mem_fut = asyncio.to_thread(_process_memory_map)

    async def _health():
        if api_client is None:
            return {}
        try:
            r = await api_client.get("/health", timeout=3.0)
            return r.json() if r.is_success else {}
        except Exception:
            return {}

    async def _tools():
        if api_client is None:
            return {}
        try:
            r = await api_client.get("/tools", timeout=3.0)
            return r.json() if r.is_success else {}
        except Exception:
            return {}

    manifests, mem_map, health_data, tools_data = await asyncio.gather(
        manifests_fut, mem_fut, _health(), _tools()
    )

    # Services registered with openagent (connected + tools returned)
    registered: set[str] = set()
    for t in tools_data.get("tools", []):
        registered.add(t.get("service", ""))

    def _entry(name: str, version: str = "?") -> dict:
        running = name in registered
        return {
            "name": name,
            "version": version,
            "status": "online" if running else "stopped",
            "memory_mb": mem_map.get(name),
            "memory_display": f"{mem_map[name]} MB" if name in mem_map else "—",
        }

    # openagent runtime — always first in Rust list
    runtime_ok = health_data.get("status") == "ok"
    runtime_entry = {
        "name": "openagent",
        "version": "?",
        "status": "online" if runtime_ok else "offline",
        "memory_mb": mem_map.get("openagent"),
        "memory_display": f"{mem_map['openagent']} MB" if "openagent" in mem_map else "—",
        "tool_count": health_data.get("tool_count", 0),
    }

    go_services: list[dict] = []
    rust_services: list[dict] = [runtime_entry]

    for name, manifest in sorted(manifests.items()):
        entry = _entry(name, manifest.get("version", "?"))
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

    return templates.TemplateResponse("dashboard.html", {
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

    return templates.TemplateResponse("_stats_cards.html", {
        "request": request,
        "stats": stats,
        "python_packages": packages,
        "services": go_services,
        "rust_services": rust_services,
    })
