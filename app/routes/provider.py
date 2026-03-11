"""Provider config API — read-only provider settings and model discovery."""

from __future__ import annotations

import httpx
from fastapi import APIRouter, Request

from openagent.providers import ProviderConfig

router = APIRouter(prefix="/api/config")


@router.get("/provider")
async def get_provider_config(request: Request):
    cfg: ProviderConfig = request.app.state.provider_config
    data = cfg.model_dump()
    data.pop("api_key", None)  # never expose key over the API
    return data


@router.get("/models")
async def get_provider_models(request: Request, base_url: str | None = None):
    """Fetch model IDs from the provider's /models endpoint (OpenAI-compatible)."""
    cfg: ProviderConfig = request.app.state.provider_config
    url = (base_url or cfg.base_url or "").rstrip("/")
    if not url or cfg.kind == "anthropic":
        return {"ok": False, "models": [], "error": "No base URL or Anthropic does not expose /models"}
    try:
        async with httpx.AsyncClient(timeout=6.0) as client:
            r = await client.get(f"{url}/models")
            r.raise_for_status()
            data = r.json()
            models = [m["id"] for m in data.get("data", []) if "id" in m]
        return {"ok": True, "models": sorted(models), "error": None}
    except Exception as exc:
        return {"ok": False, "models": [], "error": str(exc)}
