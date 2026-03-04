"""Provider config API — GET /api/config/provider + PATCH /api/config/provider"""

from __future__ import annotations

from typing import Literal

from fastapi import APIRouter, Request
from pydantic import BaseModel

from openagent.providers import ProviderConfig, get_provider

router = APIRouter(prefix="/api/config")


class ProviderPatch(BaseModel):
    kind: Literal["openai_compat", "anthropic", "openai"] | None = None
    base_url: str | None = None
    api_key: str | None = None
    model: str | None = None
    timeout: float | None = None
    max_tokens: int | None = None


@router.get("/provider")
async def get_provider_config(request: Request):
    cfg: ProviderConfig = request.app.state.provider_config
    data = cfg.model_dump()
    data.pop("api_key", None)  # never expose key over the API
    return data


@router.patch("/provider")
async def patch_provider_config(request: Request, body: ProviderPatch):
    cfg: ProviderConfig = request.app.state.provider_config
    updates = {k: v for k, v in body.model_dump().items() if v is not None}
    new_cfg = cfg.model_copy(update=updates)
    request.app.state.provider_config = new_cfg
    request.app.state.active_provider = get_provider(new_cfg)
    data = new_cfg.model_dump()
    data.pop("api_key", None)
    return {"ok": True, "config": data}
