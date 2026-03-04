"""OpenAI-compatible provider — works with any /v1/chat/completions endpoint."""

from __future__ import annotations

import json
from typing import Any, AsyncIterator

import httpx

from .base import LLMResponse, Message, ToolCall
from .config import ProviderConfig


class OpenAICompatProvider:
    """Streaming httpx client for OpenAI-compatible APIs (LM Studio, Ollama, vLLM, etc.)."""

    def __init__(self, cfg: ProviderConfig) -> None:
        self._cfg = cfg
        self._headers: dict[str, str] = {"Content-Type": "application/json"}
        if cfg.api_key:
            self._headers["Authorization"] = f"Bearer {cfg.api_key}"

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _base_payload(self, messages: list[Message]) -> dict[str, Any]:
        out: list[dict[str, Any]] = []
        for m in messages:
            if m.role == "tool":
                out.append({
                    "role": "tool",
                    "tool_call_id": m.tool_call_id,
                    "name": m.tool_name,
                    "content": m.content,
                })
            else:
                out.append({"role": m.role, "content": m.content})
        return {
            "model": self._cfg.model or "default",
            "messages": out,
            "max_tokens": self._cfg.max_tokens,
        }

    def _url(self) -> str:
        return self._cfg.base_url.rstrip("/") + "/chat/completions"

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    async def stream(
        self, messages: list[Message], **kwargs
    ) -> AsyncIterator[str]:
        payload = {**self._base_payload(messages), "stream": True}
        payload.update(kwargs)
        async with httpx.AsyncClient(timeout=self._cfg.timeout) as client:
            async with client.stream(
                "POST", self._url(), json=payload, headers=self._headers
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

    async def chat(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
        **kwargs,
    ) -> LLMResponse:
        """Non-streaming chat call with optional tool schemas.

        Parses ``tool_calls`` from the OpenAI-compatible response format.
        Works with LM Studio, Ollama (>=0.2.8), vLLM, and api.openai.com.
        """
        payload = {**self._base_payload(messages), "stream": False}
        if tools:
            payload["tools"] = tools
            payload["tool_choice"] = "auto"
        payload.update(kwargs)

        async with httpx.AsyncClient(timeout=self._cfg.timeout) as client:
            resp = await client.post(
                self._url(), json=payload, headers=self._headers
            )
            resp.raise_for_status()
            data = resp.json()

        choice = data["choices"][0]
        msg = choice.get("message", {})
        content: str = msg.get("content") or ""
        raw_calls: list[dict[str, Any]] = msg.get("tool_calls") or []

        tool_calls: list[ToolCall] = []
        for tc in raw_calls:
            fn = tc.get("function", {})
            try:
                args = json.loads(fn.get("arguments", "{}"))
            except json.JSONDecodeError:
                args = {"_raw": fn.get("arguments", "")}
            tool_calls.append(ToolCall(
                id=tc.get("id", ""),
                name=fn.get("name", ""),
                arguments=args,
            ))

        return LLMResponse(content=content, tool_calls=tool_calls)
