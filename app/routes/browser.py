"""Browser page — /browser and /api/browser/* endpoints."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Request, Response
from fastapi.responses import FileResponse, JSONResponse
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _artifacts_root(request: Request) -> Path:
    root: Path = getattr(request.app.state, "root", Path("."))
    return root / "data" / "artifacts" / "browser"


def _browser_client(request: Request):
    """Return the MCP-lite client for the browser service, or None."""
    sm = getattr(request.app.state, "service_manager", None)
    if sm is None:
        return None
    return sm.get_client("browser")


async def _call_tool(request: Request, tool: str, params: dict[str, Any]) -> dict[str, Any]:
    client = _browser_client(request)
    if client is None:
        return {"error": "Browser service is not running. Build and start services/browser first."}
    try:
        frame = await client.request(
            {"type": "tool.call", "tool": tool, "params": params},
            timeout_s=60.0,  # browser ops can be slow
        )
        result_str = getattr(frame, "result", None) or ""
        if result_str:
            try:
                return json.loads(result_str)
            except json.JSONDecodeError:
                return {"ok": True, "result": result_str}
        err = getattr(frame, "error", None)
        if err:
            return {"error": str(err)}
        return {"error": "Empty response from browser service"}
    except Exception as exc:
        return {"error": str(exc)}


# ---------------------------------------------------------------------------
# Page
# ---------------------------------------------------------------------------

@router.get("/browser")
async def browser_page(request: Request):
    return templates.TemplateResponse(request, "browser.html", {
        "request": request,
        "active": "browser",
    })


# ---------------------------------------------------------------------------
# Session list — scan artifacts dir for session directories
# ---------------------------------------------------------------------------

@router.get("/api/browser/sessions")
async def list_sessions(request: Request):
    root = _artifacts_root(request)
    if not root.exists():
        return {"sessions": []}
    sessions = []
    for session_dir in sorted(root.iterdir()):
        if not session_dir.is_dir():
            continue
        ss = session_dir / "latest.png"
        sessions.append({
            "session_id": session_dir.name,
            "has_screenshot": ss.exists(),
            "screenshot_url": f"/artifacts/browser/{session_dir.name}/latest.png" if ss.exists() else None,
            "screenshot_mtime": int(ss.stat().st_mtime * 1000) if ss.exists() else None,
        })
    return {"sessions": sessions}


# ---------------------------------------------------------------------------
# Serve screenshots directly (artifacts are NOT auto-mounted; served here)
# ---------------------------------------------------------------------------

@router.get("/artifacts/browser/{session_id}/{filename}")
async def serve_artifact(request: Request, session_id: str, filename: str):
    root = _artifacts_root(request)
    # Sanitise — no path traversal
    if ".." in session_id or ".." in filename:
        return Response(status_code=400)
    path = root / session_id / filename
    if not path.exists():
        return Response(status_code=404)
    media = "application/pdf" if filename.endswith(".pdf") else "image/png"
    return FileResponse(str(path), media_type=media)


# ---------------------------------------------------------------------------
# Tool call proxies — UI calls these directly
# ---------------------------------------------------------------------------

@router.post("/api/browser/open")
async def browser_open(request: Request):
    body = await request.json()
    url = str(body.get("url", "")).strip()
    session_id = str(body.get("session_id", "")).strip() or None
    if not url:
        return JSONResponse({"error": "url required"}, status_code=400)
    params: dict[str, Any] = {"url": url}
    if session_id:
        params["session_id"] = session_id
    return await _call_tool(request, "browser.open", params)


@router.post("/api/browser/action")
async def browser_action(request: Request):
    """Generic proxy: { tool, params } → browser service tool call."""
    body = await request.json()
    tool = str(body.get("tool", "")).strip()
    params = dict(body.get("params", {}))
    if not tool:
        return JSONResponse({"error": "tool required"}, status_code=400)
    # Restrict to browser.* tools only
    if not tool.startswith("browser."):
        return JSONResponse({"error": f"Only browser.* tools allowed, got: {tool}"}, status_code=400)
    return await _call_tool(request, tool, params)


@router.delete("/api/browser/session/{session_id}")
async def browser_close(request: Request, session_id: str):
    return await _call_tool(request, "browser.close", {"session_id": session_id})
