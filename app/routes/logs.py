"""Logs route — GET /logs, GET /stream/logs (SSE)"""

from __future__ import annotations

import asyncio
import json

from fastapi import APIRouter, Request
from fastapi.responses import StreamingResponse
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


@router.get("/logs")
async def logs_page(request: Request):
    return templates.TemplateResponse("logs.html", {
        "request": request,
        "active": "logs",
    })


@router.get("/stream/logs")
async def stream_logs(request: Request):
    """SSE endpoint — streams log records to the browser."""
    clients: set = request.app.state.log_clients
    if len(clients) >= getattr(request.app.state, "log_clients_max", 50):
        from fastapi.responses import JSONResponse
        return JSONResponse(
            status_code=503,
            content={"error": "Too many log stream clients; try again later"},
        )
    queue: asyncio.Queue[str] = asyncio.Queue(maxsize=200)
    buffer = request.app.state.log_buffer
    clients.add(queue)

    async def generate():
        # Replay recent buffer for new connections
        for entry in list(buffer):
            yield f"data: {json.dumps(entry)}\n\n"

        try:
            while True:
                try:
                    entry = await asyncio.wait_for(queue.get(), timeout=15.0)
                    yield f"data: {json.dumps(entry)}\n\n"
                except asyncio.TimeoutError:
                    # Keepalive comment — also lets Starlette detect a broken connection
                    yield ": keepalive\n\n"
        finally:
            clients.discard(queue)

    return StreamingResponse(
        generate(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "X-Accel-Buffering": "no",
        },
    )
