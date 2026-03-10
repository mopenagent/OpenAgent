"""Logs route — GET /logs"""

from __future__ import annotations

import asyncio
from pathlib import Path

from fastapi import APIRouter, Request, Query
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py

def _get_log_files(root: Path) -> list[str]:
    """Get all .log files in the logs/ directory, sorted by newest first."""
    logs_dir = root / "logs"
    if not logs_dir.exists() or not logs_dir.is_dir():
        return []
    
    log_files = list(logs_dir.glob("*.log"))
    log_files.sort(key=lambda f: f.stat().st_mtime, reverse=True)
    
    return [f.name for f in log_files]

def _read_log_file(root: Path, filename: str, max_lines: int = 2000) -> str:
    """Read the last N lines of a particular log file."""
    logs_dir = root / "logs"
    # Basic path traversal protection
    if "/" in filename or "\\" in filename or ".." in filename:
        return "Invalid file name."
        
    file_path = logs_dir / filename
    if not file_path.exists() or not file_path.is_file():
        return "Log file not found."
    
    try:
        with open(file_path, "r", encoding="utf-8", errors="replace") as f:
            lines = f.readlines()
            return "".join(lines[-max_lines:])
    except Exception as e:
        return f"Error reading log file: {e}"

@router.get("/logs")
async def logs_page(request: Request, file: str | None = Query(None)):
    root = getattr(request.app.state, "root", Path.cwd())
    
    # Run blocking directory and file parsing in threads
    log_files = await asyncio.to_thread(_get_log_files, root)
    
    selected_file = file
    if not selected_file and log_files:
        selected_file = log_files[0]
        
    log_content = ""
    if selected_file:
        log_content = await asyncio.to_thread(_read_log_file, root, selected_file)
        
    return templates.TemplateResponse("logs.html", {
        "request": request,
        "active": "logs",
        "log_files": log_files,
        "selected_file": selected_file,
        "log_content": log_content,
    })
