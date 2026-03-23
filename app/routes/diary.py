"""Diary route — GET /diary  (read-only past conversation browser)"""

from __future__ import annotations

from fastapi import APIRouter
from fastapi.requests import Request
from fastapi.templating import Jinja2Templates

from app.diary_store import DiaryStore

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


def _diary(request: Request) -> DiaryStore | None:
    return getattr(request.app.state, "diary_store", None)


@router.get("/diary")
async def diary_page(request: Request):
    diary = _diary(request)
    raw = await diary.list_sessions() if diary else []
    sessions = [
        {
            "key": s.key,
            "display_name": s.display_name,
            "platform": s.platform,
            "channel_id": s.channel_id,
            "last_active": s.last_active,
            "message_count": s.message_count,
        }
        for s in raw
    ]
    return templates.TemplateResponse(request, "diary.html", {
        "request": request,
        "active": "diary",
        "sessions": sessions,
    })
