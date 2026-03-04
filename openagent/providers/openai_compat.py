"""OpenAI-compatible provider — works with any /v1/chat/completions endpoint."""

from __future__ import annotations

import json
from typing import AsyncIterator

import httpx

from .base import Message
from .config import ProviderConfig


class OpenAICompatProvider:
    """Streaming httpx client for OpenAI-compatible APIs (LM Studio, Ollama, etc.)."""

    def __init__(self, cfg: ProviderConfig) -> None:
        self._cfg = cfg
        self._headers: dict[str, str] = {"Content-Type": "application/json"}
        if cfg.api_key:
            self._headers["Authorization"] = f"Bearer {cfg.api_key}"

    async def stream(
        self, messages: list[Message], **kwargs
    ) -> AsyncIterator[str]:
        payload: dict = {
            "model": self._cfg.model or "default",
            "messages": [{"role": m.role, "content": m.content} for m in messages],
            "stream": True,
            "max_tokens": self._cfg.max_tokens,
        }
        payload.update(kwargs)

        url = self._cfg.base_url.rstrip("/") + "/chat/completions"
        async with httpx.AsyncClient(timeout=self._cfg.timeout) as client:
            async with client.stream(
                "POST", url, json=payload, headers=self._headers
            ) as resp:
                resp.raise_for_status()
                async for line in resp.aiter_lines():
                    if not line.startswith("data: "):
                        continue
                    chunk = line[6:]
                    if chunk == "[DONE]":
                        break
                    try:
                        delta = (
                            json.loads(chunk)["choices"][0]["delta"].get("content", "")
                        )
                        if delta:
                            yield delta
                    except Exception:
                        continue

    async def complete(self, messages: list[Message], **kwargs) -> str:
        return "".join([chunk async for chunk in self.stream(messages, **kwargs)])
