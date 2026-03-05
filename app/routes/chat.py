"""Chat route — GET /chat, WS /ws/chat"""

from __future__ import annotations

import uuid

from fastapi import APIRouter, WebSocket, WebSocketDisconnect
from fastapi.requests import Request
from fastapi.templating import Jinja2Templates

from openagent.bus.bus import MessageBus
from openagent.bus.events import InboundMessage, SenderInfo
from openagent.platforms.web import WebPlatformAdapter

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


@router.get("/chat")
async def chat_page(request: Request):
    return templates.TemplateResponse("chat.html", {
        "request": request,
        "active": "chat",
    })


@router.get("/api/chat/sessions")
async def list_sessions(request: Request):
    app = request.app
    sessions = getattr(app.state, "sessions", None)
    if not sessions:
        return {"sessions": []}
    keys = await sessions.list_sessions()
    return {"sessions": keys}


@router.get("/api/chat/sessions/{session_id}/history")
async def get_history(request: Request, session_id: str):
    app = request.app
    sessions = getattr(app.state, "sessions", None)
    if not sessions:
        return {"history": []}
    history = await sessions.get_history(session_id)
    out = [
        {"role": t.role, "content": t.content, "timestamp": t.timestamp.isoformat()}
        for t in history
    ]
    return {"history": out}


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
            
            # Since web adapter does not intercept IdentityResolver,
            # we must manually assign the target session explicitly using session_key_override
            await bus.publish(InboundMessage(
                platform="web",
                channel_id=session_id,
                sender=SenderInfo(platform="web", user_id=session_id),
                content=text.strip(),
                session_key_override=session_id
            ))
    except WebSocketDisconnect:
        pass
    finally:
        web_adapter.unregister_connection(session_id)
