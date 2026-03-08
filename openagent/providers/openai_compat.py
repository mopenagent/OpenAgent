"""OpenAI-compatible provider — works with any /v1/chat/completions endpoint."""

from __future__ import annotations

import json
from typing import Any, AsyncIterator

import httpx

from .base import LLMResponse, Message, StreamEvent, ToolCall
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
            elif m.role == "assistant" and m.tool_calls:
                out.append({
                    "role": "assistant",
                    "content": m.content or "",
                    "tool_calls": [
                        {
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.name,
                                "arguments": json.dumps(tc.arguments, ensure_ascii=False),
                            },
                        }
                        for tc in m.tool_calls
                    ],
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
                    chunk = line[6:].strip()
                    if chunk == "[DONE]" or not chunk:
                        break
                    try:
                        parsed = json.loads(chunk)
                        choices = parsed.get("choices") or []
                        if not choices:
                            continue
                        delta = (
                            choices[0].get("delta") or {}
                        ).get("content", "")
                        if delta:
                            yield delta
                    except (json.JSONDecodeError, KeyError, TypeError):
                        continue

    async def stream_with_tools(
        self,
        messages: list[Message],
        *,
        tools: list[dict[str, Any]] | None = None,
        **kwargs,
    ) -> AsyncIterator[StreamEvent]:
        """Stream with tool support. Yields content deltas for UI; buffers tool_calls.

        - delta.content → StreamEvent(content=...) — stream to UI immediately
        - delta.tool_calls → buffer by index; on stream end yield tool_calls

        When ``tools`` is None or empty the request is sent without tool
        schemas and no tool buffering is performed.
        """
        payload = {
            **self._base_payload(messages),
            "stream": True,
        }
        if tools:
            payload["tools"] = tools
            payload["tool_choice"] = "auto"
        payload.update(kwargs)

        # Buffers for incremental tool_call parsing (index → partial data)
        tool_call_buffers: dict[int, dict[str, Any]] = {}
        finish_reason: str | None = None

        async with httpx.AsyncClient(timeout=self._cfg.timeout) as client:
            async with client.stream(
                "POST", self._url(), json=payload, headers=self._headers
            ) as resp:
                resp.raise_for_status()
                async for line in resp.aiter_lines():
                    if not line.startswith("data: "):
                        continue
                    raw = line[6:].strip()
                    if raw == "[DONE]" or not raw:
                        break
                    try:
                        obj = json.loads(raw)
                    except json.JSONDecodeError:
                        continue
                    choices = obj.get("choices") or []
                    if not choices:
                        continue
                    choice = choices[0]
                    delta = choice.get("delta") or {}
                    finish_reason = choice.get("finish_reason") or finish_reason

                    # Content delta — stream to UI immediately
                    content = delta.get("content")
                    if content:
                        yield StreamEvent(content=content)

                    # Tool call deltas — buffer by index
                    raw_tcs = delta.get("tool_calls") or []
                    for tc in raw_tcs:
                        idx = tc.get("index", 0)
                        if idx not in tool_call_buffers:
                            tool_call_buffers[idx] = {
                                "id": "",
                                "name": "",
                                "arguments": "",
                            }
                        buf = tool_call_buffers[idx]
                        if "id" in tc and tc["id"]:
                            buf["id"] = tc["id"]
                        fn = tc.get("function") or {}
                        if fn.get("name"):
                            buf["name"] = fn["name"]
                        if fn.get("arguments"):
                            buf["arguments"] += fn["arguments"]

                # Stream ended — emit tool_calls if any
                if tool_call_buffers and finish_reason == "tool_calls":
                    tool_calls: list[ToolCall] = []
                    for idx in sorted(tool_call_buffers.keys()):
                        buf = tool_call_buffers[idx]
                        args_raw = buf.get("arguments") or "{}"
                        try:
                            args = json.loads(args_raw)
                        except json.JSONDecodeError:
                            args = {"_raw": args_raw}
                        tool_calls.append(
                            ToolCall(
                                id=buf.get("id") or f"call_{idx}",
                                name=buf.get("name") or "",
                                arguments=args,
                            )
                        )
                    yield StreamEvent(tool_calls=tool_calls, finish_reason="tool_calls")
                elif finish_reason:
                    yield StreamEvent(finish_reason=finish_reason)

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

        choices = data.get("choices") or []
        if not choices:
            return LLMResponse(content="", tool_calls=[])
        choice = choices[0]
        msg = choice.get("message") or {}
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
