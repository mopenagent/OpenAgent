"""LLM health API — GET /api/llm/health"""

from __future__ import annotations

import os

import httpx
from fastapi import APIRouter

router = APIRouter(prefix="/api/llm")

_LLM_BASE = os.environ.get("OPENAGENT_LLM_BASE_URL", "http://100.74.210.70:1234/v1").rstrip("/")


@router.get("/health")
async def llm_health():
    try:
        async with httpx.AsyncClient(timeout=4.0) as client:
            r = await client.get(f"{_LLM_BASE}/models")
            r.raise_for_status()
            data = r.json()
            models = [m["id"] for m in data.get("data", []) if "id" in m]
        return {"ok": True, "models": sorted(models), "base": _LLM_BASE, "error": None}
    except Exception as exc:
        return {"ok": False, "models": [], "base": _LLM_BASE, "error": str(exc)}
