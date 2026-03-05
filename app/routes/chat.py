"""Chat route — GET /chat, WS /ws/chat, SSE /stream/session, POST /api/chat/sessions/{id}/send"""

from __future__ import annotations

import asyncio
import json
import uuid

from fastapi import APIRouter, WebSocket, WebSocketDisconnect
from fastapi.requests import Request
from fastapi.responses import StreamingResponse
from fastapi.templating import Jinja2Templates

from openagent.bus.bus import MessageBus
from openagent.bus.events import InboundMessage, OutboundMessage, SenderInfo
from openagent.platforms.web import WebPlatformAdapter

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


def _get_sessions(request: Request):
    """Return the SessionManager from app state (handles both attribute names)."""
    app = request.app
    return getattr(app.state, "session_manager", None) or getattr(app.state, "sessions", None)


@router.get("/chat")
async def chat_page(request: Request):
    return templates.TemplateResponse("chat.html", {
        "request": request,
        "active": "chat",
    })


@router.get("/api/chat/sessions")
async def list_sessions(request: Request):
    """Return sessions with platform metadata for the operator sidebar."""
    sessions = _get_sessions(request)
    if not sessions:
        return {"sessions": []}

    keys = await sessions.list_sessions()
    result = []
    for key in keys:
        if key.startswith("user:"):
            links = await sessions.get_identity_links(key)
            if links:
                primary = links[0]  # already sorted newest-active first
                result.append({
                    "key": key,
                    "platform": primary["platform"],
                    "channel_id": primary["channel_id"],
                    "platforms": links,
                })
                continue
        # Fallback: derive platform from key prefix (e.g. "telegram:12345")
        if ":" in key:
            platform, channel_id = key.split(":", 1)
        else:
            platform, channel_id = "unknown", key
        result.append({
            "key": key,
            "platform": platform,
            "channel_id": channel_id,
            "platforms": [{"platform": platform, "channel_id": channel_id, "last_active": None}],
        })

    return {"sessions": result}


@router.get("/api/chat/sessions/{session_id}/history")
async def get_history(request: Request, session_id: str):
    sessions = _get_sessions(request)
    if not sessions:
        return {"history": []}
    history = await sessions.get_history(session_id)
    out = [
        {"role": t.role, "content": t.content, "timestamp": t.timestamp.isoformat()}
        for t in history
    ]
    return {"history": out}


@router.delete("/api/chat/sessions/{session_id}")
async def delete_session(request: Request, session_id: str):
    """Soft-delete: hide session from sidebar while keeping turns for logs."""
    sessions = _get_sessions(request)
    if not sessions:
        return {"error": "session manager unavailable"}
    await sessions.hide_session(session_id)
    return {"ok": True}


@router.post("/api/chat/sessions/{session_id}/send")
async def direct_send(request: Request, session_id: str):
    """Operator direct reply — bypasses the agent loop, goes straight to platform adapter."""
    body = await request.json()
    content = str(body.get("content", "")).strip()
    if not content:
        return {"error": "content required"}

    bus: MessageBus = request.app.state.bus
    sessions = _get_sessions(request)

    platform: str
    channel_id: str

    if session_id.startswith("user:") and sessions:
        links = await sessions.get_identity_links(session_id)
        if not links:
            return {"error": "no platform links found for this session"}
        primary = links[0]
        platform = primary["platform"]
        channel_id = primary["channel_id"]
    elif ":" in session_id:
        platform, channel_id = session_id.split(":", 1)
    else:
        return {"error": "cannot determine platform from session id"}

    if platform == "web":
        return {"error": "use WebSocket for web sessions"}

    msg = OutboundMessage(
        platform=platform,
        channel_id=channel_id,
        content=content,
        session_key=session_id,
    )
    await bus.dispatch(msg)
    return {"ok": True, "platform": platform, "channel_id": channel_id}


@router.get("/stream/session/{session_key:path}")
async def stream_session(request: Request, session_key: str):
    """SSE endpoint — streams real-time inbound+outbound events for a session."""
    bus: MessageBus = request.app.state.bus
    q = bus.subscribe(session_key)

    async def event_generator():
        try:
            while True:
                if await request.is_disconnected():
                    break
                try:
                    event = await asyncio.wait_for(q.get(), timeout=15.0)
                    if event is None:  # bus shutdown sentinel
                        break
                    yield f"data: {json.dumps(event)}\n\n"
                except asyncio.TimeoutError:
                    yield ": keepalive\n\n"
        finally:
            bus.unsubscribe(session_key, q)

    return StreamingResponse(
        event_generator(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "X-Accel-Buffering": "no",
        },
    )


@router.websocket("/ws/chat")
async def chat_ws(ws: WebSocket):
    await ws.accept()

    app = ws.app
    bus: MessageBus = app.state.bus
    web_adapter: WebPlatformAdapter = app.state.web_platform

    # Prefer requested session ID, otherwise generate unique tab ID
    requested_session = ws.query_params.get("session_id")
    if requested_session:
        session_id = requested_session
    else:
        session_id = f"web:{uuid.uuid4().hex[:12]}"

    async def _deliver(content: str, stream_chunk: bool = False) -> None:
        await ws.send_json({
            "role": "agent",
            "content": content,
            "stream_chunk": stream_chunk,
        })

    web_adapter.register_connection(session_id, _deliver)
    # Tell the browser its session ID so it can display it.
    await ws.send_json({"session_id": session_id})

    try:
        while True:
            text = await ws.receive_text()
            if not text.strip():
                continue

            await bus.publish(InboundMessage(
                platform="web",
                channel_id=session_id,
                sender=SenderInfo(platform="web", user_id=session_id),
                content=text.strip(),
                session_key_override=session_id,
            ))
    except WebSocketDisconnect:
        pass
    finally:
        web_adapter.unregister_connection(session_id)
