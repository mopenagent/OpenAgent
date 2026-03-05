"""Chat route — GET /chat, WS /ws/chat"""

from __future__ import annotations

import uuid

from fastapi import APIRouter, WebSocket, WebSocketDisconnect
from fastapi.requests import Request
from fastapi.templating import Jinja2Templates

from openagent.bus.bus import MessageBus
from openagent.bus.events import InboundMessage, SenderInfo
from openagent.channels.web import WebChannelAdapter

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


@router.get("/chat")
async def chat_page(request: Request):
    return templates.TemplateResponse("chat.html", {
        "request": request,
        "active": "chat",
    })


@router.websocket("/ws/chat")
async def chat_ws(ws: WebSocket):
    await ws.accept()

    app = ws.app
    bus: MessageBus = app.state.bus
    web_adapter: WebChannelAdapter = app.state.web_channel

    # Unique ID for this browser tab — becomes the session key "web:<id>"
    session_id = uuid.uuid4().hex[:12]

    async def _deliver(content: str) -> None:
        await ws.send_json({"role": "agent", "content": content})

    web_adapter.register_connection(session_id, _deliver)
    # Tell the browser its session ID so it can display it.
    await ws.send_json({"session_id": session_id})

    try:
        while True:
            text = await ws.receive_text()
            if not text.strip():
                continue
            await bus.publish(InboundMessage(
                channel="web",
                chat_id=session_id,
                sender=SenderInfo(platform="web", user_id=session_id),
                content=text.strip(),
            ))
    except WebSocketDisconnect:
        pass
    finally:
        web_adapter.unregister_connection(session_id)
