"""Anthropic Claude provider — Anthropic Messages API with streaming."""

from __future__ import annotations

import json
from typing import AsyncIterator

import httpx

from .base import Message
from .config import ProviderConfig

_ANTHROPIC_BASE = "https://api.anthropic.com"
_ANTHROPIC_VERSION = "2023-06-01"


class AnthropicProvider:
    """Streaming httpx client for the Anthropic Messages API."""

    def __init__(self, cfg: ProviderConfig) -> None:
        self._cfg = cfg
        self._headers = {
            "Content-Type": "application/json",
            "anthropic-version": _ANTHROPIC_VERSION,
            "x-api-key": cfg.api_key,
        }

    async def stream(
        self, messages: list[Message], **kwargs
    ) -> AsyncIterator[str]:
        system = next(
            (m.content for m in messages if m.role == "system"), None
        )
        user_msgs = [
            {"role": m.role, "content": m.content}
            for m in messages
            if m.role != "system"
        ]

        payload: dict = {
            "model": self._cfg.model or "claude-sonnet-4-6",
            "messages": user_msgs,
            "stream": True,
            "max_tokens": self._cfg.max_tokens,
        }
        if system:
            payload["system"] = system
        payload.update(kwargs)

        base = (self._cfg.base_url or _ANTHROPIC_BASE).rstrip("/")
        # If the base_url already ends with /v1 don't double it
        url = base + "/messages" if base.endswith("/v1") else base + "/v1/messages"

        async with httpx.AsyncClient(timeout=self._cfg.timeout) as client:
            async with client.stream(
                "POST", url, json=payload, headers=self._headers
            ) as resp:
                resp.raise_for_status()
                async for line in resp.aiter_lines():
                    if not line.startswith("data: "):
                        continue
                    chunk = line[6:]
                    try:
                        event = json.loads(chunk)
                        if event.get("type") == "content_block_delta":
                            delta = event.get("delta", {}).get("text", "")
                            if delta:
                                yield delta
                    except Exception:
                        continue

    async def complete(self, messages: list[Message], **kwargs) -> str:
        return "".join([chunk async for chunk in self.stream(messages, **kwargs)])
