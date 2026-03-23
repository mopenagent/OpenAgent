"""Logs route — GET /logs"""

from __future__ import annotations

import asyncio
import json
from datetime import datetime
from pathlib import Path
from typing import Any

from fastapi import APIRouter, Request, Query
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


def _summarize_payload(payload: dict[str, Any]) -> str:
    """Build a compact one-line summary for a JSON log record."""
    if resource_logs := payload.get("resourceLogs"):
        try:
            record = resource_logs[0]["scopeLogs"][0]["logRecords"][0]
            severity = record.get("severityText", "LOG")
            body = record.get("body", {})
            message = body.get("stringValue") if isinstance(body, dict) else str(body)
            attrs = {
                item.get("key"): item.get("value", {}).get("stringValue")
                for item in record.get("attributes", [])
                if isinstance(item, dict)
            }
            target = attrs.get("target") or record.get("target") or "log"
            return f"{severity} {target}: {message}"
        except (IndexError, KeyError, TypeError, AttributeError):
            pass

    if resource_spans := payload.get("resourceSpans"):
        try:
            span = resource_spans[0]["scopeSpans"][0]["spans"][0]
            name = span.get("name", "span")
            status = span.get("status", {}).get("code", "?")
            return f"TRACE {name} status={status}"
        except (IndexError, KeyError, TypeError, AttributeError):
            pass

    keys = ", ".join(list(payload.keys())[:4])
    return f"JSON record: {keys}" if keys else "JSON record"


def _build_log_entries(lines: list[str], filename: str) -> list[dict[str, str]]:
    """Convert log lines into expandable UI rows."""
    entries: list[dict[str, str]] = []
    for idx, line in enumerate(lines, start=1):
        stripped = line.strip()
        if not stripped:
            continue
        if filename.endswith(".jsonl"):
            try:
                payload = json.loads(stripped)
            except json.JSONDecodeError:
                entries.append({
                    "summary": f"Line {idx}",
                    "pretty": stripped,
                })
                continue
            entries.append({
                "summary": _summarize_payload(payload),
                "pretty": json.dumps(payload, indent=2, ensure_ascii=True),
            })
            continue
        entries.append({
            "summary": f"Line {idx}",
            "pretty": stripped,
        })
    return entries


def _get_log_files(root: Path) -> list[str]:
    """Get supported log files from the latest modified day, newest first."""
    logs_dir = root / "logs"
    if not logs_dir.exists() or not logs_dir.is_dir():
        return []

    log_files = [
        f for f in logs_dir.iterdir()
        if f.is_file()
        and (
            f.suffix in {".log", ".jsonl"}
            or ".logs." in f.name
            or ".metrics." in f.name
            or ".traces." in f.name
        )
    ]
    if not log_files:
        return []

    latest_day = max(
        datetime.fromtimestamp(f.stat().st_mtime).date()
        for f in log_files
    )
    log_files = [
        f for f in log_files
        if datetime.fromtimestamp(f.stat().st_mtime).date() == latest_day
    ]
    log_files.sort(key=lambda f: f.stat().st_mtime, reverse=True)

    return [f.name for f in log_files]

def _read_log_file(root: Path, filename: str, max_lines: int = 2000) -> list[dict[str, str]]:
    """Read the last N lines of a particular log file."""
    logs_dir = root / "logs"
    # Basic path traversal protection
    if "/" in filename or "\\" in filename or ".." in filename:
        return [{"summary": "Invalid file name.", "pretty": "Invalid file name."}]
        
    file_path = logs_dir / filename
    if not file_path.exists() or not file_path.is_file():
        return [{"summary": "Log file not found.", "pretty": "Log file not found."}]
    
    try:
        with open(file_path, "r", encoding="utf-8", errors="replace") as f:
            lines = f.readlines()
            entries = _build_log_entries(lines[-max_lines:], filename)
            entries.reverse()
            return entries
    except Exception as e:
        message = f"Error reading log file: {e}"
        return [{"summary": message, "pretty": message}]

@router.get("/logs")
async def logs_page(request: Request, file: str | None = Query(None)):
    root = getattr(request.app.state, "root", Path.cwd())
    
    # Run blocking directory and file parsing in threads
    log_files = await asyncio.to_thread(_get_log_files, root)
    
    selected_file = file
    if not selected_file and log_files:
        selected_file = log_files[0]
        
    log_entries: list[dict[str, str]] = []
    if selected_file:
        log_entries = await asyncio.to_thread(_read_log_file, root, selected_file)
        
    return templates.TemplateResponse(request, "logs.html", {
        "request": request,
        "active": "logs",
        "log_files": log_files,
        "selected_file": selected_file,
        "log_entries": log_entries,
    })
