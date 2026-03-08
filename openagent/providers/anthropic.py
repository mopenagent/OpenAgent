"""Anthropic Claude provider — Anthropic Messages API with streaming and tool use."""

from __future__ import annotations

import json
from typing import Any, AsyncIterator

import httpx

from .base import LLMResponse, Message, StreamEvent, ToolCall
from .config import ProviderConfig

_ANTHROPIC_BASE = "https://api.anthropic.com"
_ANTHROPIC_VERSION = "2023-06-01"


class AnthropicProvider:
    """httpx client for the Anthropic Messages API."""

    def __init__(self, cfg: ProviderConfig) -> None:
        self._cfg = cfg
        self._headers = {
            "Content-Type": "application/json",
            "anthropic-version": _ANTHROPIC_VERSION,
            "x-api-key": cfg.api_key,
        }

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _url(self) -> str:
        base = (self._cfg.base_url or _ANTHROPIC_BASE).rstrip("/")
        return base + "/messages" if base.endswith("/v1") else base + "/v1/messages"

    def _split_messages(
        self, messages: list[Message]
    ) -> tuple[str | None, list[dict[str, Any]]]:
        """Split out the system prompt; convert tool messages to Anthropic format."""
        system = next((m.content for m in messages if m.role == "system"), None)
        out: list[dict[str, Any]] = []
        for m in messages:
            if m.role == "system":
                continue
            if m.role == "tool":
                # Anthropic expects tool results as user messages with a specific structure
                out.append({
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": m.tool_call_id,
                            "content": m.content,
                        }
                    ],
                })
            else:
                out.append({"role": m.role, "content": m.content})
        return system, out

    def _tools_to_anthropic(
        self, tools: list[dict[str, Any]]
    ) -> list[dict[str, Any]]:
        """Convert OpenAI-style tool schemas to Anthropic format."""
        result = []
        for t in tools:
            fn = t.get("function", t)  # handle both wrapped and flat formats
            result.append({
                "name": fn.get("name", ""),
                "description": fn.get("description", ""),
                "input_schema": fn.get("parameters", fn.get("params", {})),
            })
        return result

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    async def stream(
        self, messages: list[Message], **kwargs
    ) -> AsyncIterator[str]:
        system, user_msgs = self._split_messages(messages)
        payload: dict[str, Any] = {
            "model": self._cfg.model or "claude-sonnet-4-6",
            "messages": user_msgs,
            "stream": True,
            "max_tokens": self._cfg.max_tokens,
        }
        if system:
            payload["system"] = system
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

    async def chat(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
        **kwargs,
    ) -> LLMResponse:
        """Non-streaming chat with optional tool_use support (Anthropic format)."""
        system, user_msgs = self._split_messages(messages)
        payload: dict[str, Any] = {
            "model": self._cfg.model or "claude-sonnet-4-6",
            "messages": user_msgs,
            "max_tokens": self._cfg.max_tokens,
        }
        if system:
            payload["system"] = system
        if tools:
            payload["tools"] = self._tools_to_anthropic(tools)
        payload.update(kwargs)

        async with httpx.AsyncClient(timeout=self._cfg.timeout) as client:
            resp = await client.post(
                self._url(), json=payload, headers=self._headers
            )
            resp.raise_for_status()
            data = resp.json()

        content = ""
        tool_calls: list[ToolCall] = []
        for block in data.get("content", []):
            if block.get("type") == "text":
                content += block.get("text", "")
            elif block.get("type") == "tool_use":
                tool_calls.append(ToolCall(
                    id=block.get("id", ""),
                    name=block.get("name", ""),
                    arguments=block.get("input", {}),
                ))

        return LLMResponse(content=content, tool_calls=tool_calls)

    async def stream_with_tools(
        self,
        messages: list[Message],
        *,
        tools: list[dict[str, Any]] | None = None,
        **kwargs,
    ) -> AsyncIterator[StreamEvent]:
        """Uniform streaming interface — wraps chat() for Anthropic.

        Anthropic's SSE format for tool_use is complex (separate content block
        events per tool); we fall back to a single non-streaming chat() call
        and wrap the result as StreamEvents.  The agent loop is identical
        regardless of whether real token-by-token streaming occurs here.
        """
        response = await self.chat(messages, tools=tools, **kwargs)
        if response.content:
            yield StreamEvent(content=response.content)
        if response.tool_calls:
            yield StreamEvent(tool_calls=response.tool_calls, finish_reason="tool_calls")
