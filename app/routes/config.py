"""Config route — GET /config (read-only)"""

from __future__ import annotations

from pathlib import Path

import yaml
from fastapi import APIRouter, Request
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


def _load_config(root: Path) -> tuple[str, str | None]:
    """Return (raw_yaml, error_message)."""
    config_file = root / "config" / "openagent.yaml"
    if not config_file.exists():
        example = root / "config" / "openagent.yaml.example"
        if example.exists():
            return example.read_text(), "Using openagent.yaml.example — no active config found."
        return "", "No config/openagent.yaml found. Create one to configure the agent."
    try:
        raw = config_file.read_text()
        yaml.safe_load(raw)  # validate only
        return raw, None
    except yaml.YAMLError as exc:
        return config_file.read_text(), f"YAML parse error: {exc}"


@router.get("/config")
async def config_page(request: Request):
    raw, error = _load_config(request.app.state.root)
    cfg = request.app.state.provider_config
    return templates.TemplateResponse("config.html", {
        "request": request,
        "active": "config",
        "config_raw": raw,
        "config_error": error,
        "provider": cfg,
    })
