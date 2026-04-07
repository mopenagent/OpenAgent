"""MCP-lite Tester — GET /mcp-tester + POST /api/mcp-tester/*

Sends MCP-lite frames directly over TCP to service ports.
Services are now pure TCP daemons (no Unix sockets).

Port map (from services/*/service.json "address" field):
  browser :9001  channels :9002  cortex :9003  guard     :9004
  memory  :9005  research :9006  sandbox:9007  stt       :9008
  tts     :9009  validator:9010  whatsapp:9011

In Docker the host is reached via host.docker.internal (set by docker-compose
extra_hosts). Locally it resolves to 127.0.0.1.
"""

from __future__ import annotations

import asyncio
import json
import time
import uuid
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py

# Host to reach services — override via OPENAGENT_SERVICES_HOST env var.
# In Docker: host.docker.internal; bare host: 127.0.0.1
import os
SERVICES_HOST = os.environ.get("OPENAGENT_SERVICES_HOST", "host.docker.internal")


# ---------------------------------------------------------------------------
# Service discovery
# ---------------------------------------------------------------------------

def _discover_services(root: Path) -> list[dict[str, Any]]:
    """Scan services/*/service.json and return manifests with address fields."""
    services_dir = root / "services"
    result: list[dict[str, Any]] = []

    if not services_dir.exists():
        return result

    for svc_dir in sorted(services_dir.iterdir()):
        manifest_path = svc_dir / "service.json"
        if not manifest_path.exists():
            continue
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (json.JSONDecodeError, OSError):
            continue

        address = manifest.get("address", "")
        if not address:
            continue

        # Parse port from "0.0.0.0:9001"
        try:
            port = int(address.split(":")[-1])
        except ValueError:
            continue

        result.append({
            "name": manifest.get("name", svc_dir.name),
            "description": manifest.get("description", ""),
            "version": manifest.get("version", ""),
            "runtime": manifest.get("runtime", "rust"),
            "address": address,
            "port": port,
            "tools": manifest.get("tools", []),
        })

    return result


# ---------------------------------------------------------------------------
# MCP-lite TCP client
# ---------------------------------------------------------------------------

async def _mcp_call(
    host: str,
    port: int,
    frame: dict,
    timeout: float = 30.0,
) -> dict:
    """Open a TCP connection, send one MCP-lite frame, read back one response."""
    try:
        reader, writer = await asyncio.wait_for(
            asyncio.open_connection(host, port, limit=8 * 1024 * 1024),
            timeout=5.0,
        )
    except ConnectionRefusedError:
        return {"error": f"Connection refused on {host}:{port} — is the service running?"}
    except asyncio.TimeoutError:
        return {"error": f"Timed out connecting to {host}:{port}"}
    except OSError as e:
        return {"error": f"Cannot connect to {host}:{port}: {e}"}

    try:
        raw = json.dumps(frame) + "\n"
        writer.write(raw.encode())
        await writer.drain()

        line = await asyncio.wait_for(reader.readline(), timeout=timeout)
        if not line:
            return {"error": "Service closed connection without responding."}

        return json.loads(line.decode().strip())

    except asyncio.TimeoutError:
        return {"error": f"Tool call timed out after {timeout}s"}
    except json.JSONDecodeError as e:
        return {"error": f"Invalid JSON in response: {e}"}
    except Exception as e:
        return {"error": str(e)}
    finally:
        try:
            writer.close()
            await writer.wait_closed()
        except Exception:
            pass


async def _is_reachable(host: str, port: int) -> bool:
    """Quick TCP connect check — does not send any frame."""
    try:
        _, writer = await asyncio.wait_for(
            asyncio.open_connection(host, port),
            timeout=1.0,
        )
        writer.close()
        await writer.wait_closed()
        return True
    except Exception:
        return False


# ---------------------------------------------------------------------------
# Routes
# ---------------------------------------------------------------------------

@router.get("/mcp-tester")
async def mcp_tester_page(request: Request):
    return templates.TemplateResponse(
        request, "mcp_tester.html", {"request": request, "active": "mcp-tester"},
    )


@router.get("/api/mcp-tester/services")
async def mcp_tester_services(request: Request):
    """Discover services and check TCP reachability."""
    root: Path = request.app.state.root
    services = _discover_services(root)

    # Check all ports concurrently
    checks = await asyncio.gather(*[
        _is_reachable(SERVICES_HOST, svc["port"]) for svc in services
    ])
    for svc, reachable in zip(services, checks):
        svc["socket_exists"] = reachable  # field name kept for template compatibility
        svc["connect_host"] = SERVICES_HOST

    return JSONResponse({"services": services, "services_host": SERVICES_HOST})


@router.post("/api/mcp-tester/ping")
async def mcp_tester_ping(request: Request):
    try:
        body = await request.json()
        port = int(body.get("port", 0))
        if not port:
            return JSONResponse({"error": "port is required"}, status_code=400)
        frame = {"id": uuid.uuid4().hex, "type": "ping"}
        response = await _mcp_call(SERVICES_HOST, port, frame, timeout=5.0)
        return JSONResponse({"request_frame": frame, "response_frame": response})
    except Exception as e:
        return JSONResponse({"request_frame": None, "response_frame": {"error": str(e)}})


@router.post("/api/mcp-tester/tools-list")
async def mcp_tester_tools_list(request: Request):
    try:
        body = await request.json()
        port = int(body.get("port", 0))
        if not port:
            return JSONResponse({"error": "port is required"}, status_code=400)
        frame = {"id": uuid.uuid4().hex, "type": "tools.list"}
        response = await _mcp_call(SERVICES_HOST, port, frame, timeout=10.0)
        return JSONResponse({"request_frame": frame, "response_frame": response})
    except Exception as e:
        return JSONResponse({"request_frame": None, "response_frame": {"error": str(e)}})


@router.post("/api/mcp-tester/call")
async def mcp_tester_call(request: Request):
    try:
        body = await request.json()
        port = int(body.get("port", 0))
        tool = body.get("tool", "")
        params = body.get("params", {})
        timeout_s = float(body.get("timeout_s", 30.0))
        if not port or not tool:
            return JSONResponse({"error": "port and tool are required"}, status_code=400)
        frame = {"id": uuid.uuid4().hex, "type": "tool.call", "tool": tool, "params": params}
        t0 = time.monotonic()
        response = await _mcp_call(SERVICES_HOST, port, frame, timeout=timeout_s)
        duration_ms = round((time.monotonic() - t0) * 1000)
        return JSONResponse({"request_frame": frame, "response_frame": response, "duration_ms": duration_ms})
    except Exception as e:
        return JSONResponse({"request_frame": None, "response_frame": {"error": str(e)}, "duration_ms": None})
