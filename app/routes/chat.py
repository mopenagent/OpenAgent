"""Chat route — GET /chat, WS /ws/chat, POST /api/chat/sessions/{id}/send"""

from __future__ import annotations

import json
import uuid

import httpx
from fastapi import APIRouter, WebSocket, WebSocketDisconnect
from fastapi.requests import Request
from fastapi.responses import JSONResponse
from fastapi.templating import Jinja2Templates

from app.diary_store import DiaryStore

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


def _diary(request: Request) -> DiaryStore | None:
    return getattr(request.app.state, "diary_store", None)


@router.get("/chat")
async def chat_page(request: Request):
    return templates.TemplateResponse("chat.html", {
        "request": request,
        "active": "chat",
    })


@router.get("/api/chat/sessions")
async def list_sessions(request: Request):
    """Return sessions from diary directories for the sidebar."""
    diary = _diary(request)
    if not diary:
        return {"sessions": []}
    sessions = await diary.list_sessions()
    return {"sessions": [
        {
            "key": s.key,
            "display_name": s.display_name,
            "platform": s.platform,
            "channel_id": s.channel_id,
            "platforms": [{"platform": s.platform, "channel_id": s.channel_id, "last_active": s.last_active}],
        }
        for s in sessions
    ]}


@router.get("/api/chat/sessions/{session_id:path}/history")
async def get_history(request: Request, session_id: str):
    """Return conversation history from diary markdown files."""
    diary = _diary(request)
    if not diary:
        return {"history": []}
    messages = diary.get_history(session_id)
    return {"history": [
        {"role": m.role, "content": m.content, "timestamp": m.timestamp}
        for m in messages
    ]}


@router.patch("/api/chat/sessions/{session_id:path}/name")
async def rename_session(request: Request, session_id: str):
    """Set a human-readable name for a session."""
    diary = _diary(request)
    if not diary:
        return JSONResponse({"error": "diary store unavailable"}, status_code=503)
    body = await request.json()
    name = str(body.get("name", "")).strip()
    if not name:
        return JSONResponse({"error": "name required"}, status_code=400)
    diary.set_contact_name(session_id, name)
    return {"ok": True, "session_id": session_id, "name": name}


@router.delete("/api/chat/sessions/{session_id:path}")
async def delete_session(request: Request, session_id: str):
    """Soft-delete: hide session from sidebar."""
    diary = _diary(request)
    if not diary:
        return {"error": "diary store unavailable"}
    await diary.hide_session(session_id)
    return {"ok": True}


@router.post("/api/chat/sessions/{session_id:path}/send")
async def direct_send(request: Request, session_id: str):
    """Operator direct reply — calls Rust POST /tool/channel.send."""
    body = await request.json()
    content = str(body.get("content", "")).strip()
    if not content:
        return {"error": "content required"}

    # Resolve platform + channel from session_id
    if "://" in session_id:
        platform = session_id.split("://")[0]
        # channel is the full platform://chatID part (before the :senderID suffix)
        # For sending we need the chatID, not senderID
        rest = session_id.split("://", 1)[1]
        parts = rest.split(":")
        chat_id = parts[0] if "@" in parts[0] else rest
        channel_uri = f"{platform}://{chat_id}"
    elif ":" in session_id:
        platform, channel_id = session_id.split(":", 1)
        channel_uri = f"{platform}://{channel_id}"
    else:
        return {"error": "cannot determine platform from session id"}

    if platform == "web":
        return {"error": "use WebSocket for web sessions"}

    api_client: httpx.AsyncClient = request.app.state.api_client
    try:
        if platform == "whatsapp":
            chat_id = channel_uri.removeprefix("whatsapp://")
            resp = await api_client.post("/tool/whatsapp.send_text", content=json.dumps({
                "chat_id": chat_id,
                "text": content,
            }), headers={"Content-Type": "application/json"})
        else:
            resp = await api_client.post("/tool/channel.send", content=json.dumps({
                "address": channel_uri,
                "content": content,
            }), headers={"Content-Type": "application/json"})
        return {"ok": resp.is_success, "platform": platform, "channel_uri": channel_uri}
    except Exception as e:
        return {"error": str(e)}


@router.websocket("/ws/chat")
async def chat_ws(ws: WebSocket):
    await ws.accept()

    app = ws.app
    api_client: httpx.AsyncClient = app.state.api_client

    # Prefer requested session ID, otherwise generate unique tab ID.
    requested_session = ws.query_params.get("session_id")
    if requested_session:
        session_id = requested_session
    else:
        session_id = f"web:{uuid.uuid4().hex[:12]}"

    # Tell the browser its session ID.
    await ws.send_json({"session_id": session_id})

    try:
        while True:
            text = await ws.receive_text()
            if not text.strip():
                continue

            try:
                resp = await api_client.post("/step", json={
                    "platform": "web",
                    "channel_id": session_id,
                    "session_id": session_id,
                    "user_input": text.strip(),
                })
                resp.raise_for_status()
                data = resp.json()
                response_text = data.get("response_text", "")
                await ws.send_json({"role": "agent", "content": response_text})
            except httpx.HTTPStatusError as e:
                await ws.send_json({"role": "error", "content": f"Agent error: {e.response.status_code}"})
            except Exception as e:
                await ws.send_json({"role": "error", "content": str(e)})
    except WebSocketDisconnect:
        pass
