"""Research route — GET /research (read-only research DAG browser)"""

from __future__ import annotations

from pathlib import Path

import aiosqlite
from fastapi import APIRouter
from fastapi.requests import Request
from fastapi.responses import JSONResponse, PlainTextResponse
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


async def _get_researches(db_path: Path) -> list[dict]:
    """Read all researches from SQLite, newest first."""
    if not db_path.exists():
        return []
    async with aiosqlite.connect(db_path) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute(
            "SELECT * FROM researches ORDER BY updated_at DESC"
        ) as cursor:
            return [dict(r) for r in await cursor.fetchall()]


async def _get_tasks(db_path: Path, research_id: str) -> list[dict]:
    """Read all tasks for a research, ordered by creation time."""
    if not db_path.exists():
        return []
    async with aiosqlite.connect(db_path) as db:
        db.row_factory = aiosqlite.Row
        async with db.execute(
            "SELECT * FROM research_tasks WHERE research_id = ? ORDER BY created_at ASC",
            (research_id,),
        ) as cursor:
            return [dict(r) for r in await cursor.fetchall()]


@router.get("/research")
async def research_page(request: Request):
    root: Path = request.app.state.root
    db_path = root / "data" / "research.db"
    researches = await _get_researches(db_path)
    return templates.TemplateResponse(
        request,
        "research.html",
        {
            "request": request,
            "active": "research",
            "researches": researches,
        },
    )


@router.get("/api/research/{research_id}/tasks")
async def research_tasks_api(request: Request, research_id: str):
    root: Path = request.app.state.root
    db_path = root / "data" / "research.db"
    tasks = await _get_tasks(db_path, research_id)
    return JSONResponse(tasks)


@router.get("/api/research/{research_id}/snapshot")
async def research_snapshot_api(request: Request, research_id: str):
    root: Path = request.app.state.root
    snapshot_path = root / "data" / "research" / research_id / "snapshot.md"
    if snapshot_path.exists():
        return PlainTextResponse(snapshot_path.read_text())
    return PlainTextResponse("No snapshot available yet.")
