"""Provider adapter that routes LLM calls through the Cortex MCP-lite service."""

from __future__ import annotations

import json
from collections.abc import Callable
from typing import Any, AsyncIterator

from openagent.platforms.mcplite import McpLiteClient
from openagent.providers.base import LLMResponse, Message, StreamEvent
from openagent.services import protocol as proto


class CortexProvider:
    """Provider-compatible adapter that delegates chat completion to Cortex.

    Phase 1 uses ``cortex.step`` for one-shot response generation. Tool calling
    remains disabled here; the adapter returns plain text only.
    """

    def __init__(
        self,
        *,
        get_client: Callable[[], McpLiteClient | None],
        default_agent_name: str = "",
    ) -> None:
        self._get_client = get_client
        self._default_agent_name = default_agent_name

    async def stream(
        self, messages: list[Message], **kwargs: Any
    ) -> AsyncIterator[str]:
        response = await self.chat(messages, **kwargs)
        if response.content:
            yield response.content

    async def complete(self, messages: list[Message], **kwargs: Any) -> str:
        response = await self.chat(messages, **kwargs)
        return response.content

    async def chat(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        del tools  # Phase 1 Cortex does not support tool calling yet.

        client = self._get_client()
        if client is None:
            raise RuntimeError("cortex service is not running")

        session_key = str(kwargs.get("session_key") or "").strip()
        if not session_key:
            raise RuntimeError("cortex provider requires session_key")

        user_input = _render_transcript(messages)
        payload: dict[str, Any] = {
            "type": "tool.call",
            "tool": "cortex.step",
            "params": {
                "session_id": session_key,
                "user_input": user_input,
            },
        }
        if self._default_agent_name:
            payload["params"]["agent_name"] = self._default_agent_name

        frame = await client.request(payload, timeout_s=90.0)
        if not isinstance(frame, proto.ToolResultResponse):
            raise RuntimeError(f"unexpected cortex response: {type(frame).__name__}")
        if frame.error:
            raise RuntimeError(frame.error)

        parsed = json.loads(frame.result or "{}")
        return LLMResponse(content=str(parsed.get("response_text") or ""))

    async def stream_with_tools(
        self,
        messages: list[Message],
        *,
        tools: list[dict[str, Any]] | None = None,
        **kwargs: Any,
    ) -> AsyncIterator[StreamEvent]:
        response = await self.chat(messages, tools=tools, **kwargs)
        if response.content:
            yield StreamEvent(content=response.content)


def _render_transcript(messages: list[Message]) -> str:
    """Flatten message history into a deterministic text transcript for Phase 1."""
    parts: list[str] = []
    for message in messages:
        if message.role == "system":
            continue
        role = message.role.upper()
        if message.role == "tool" and message.tool_name:
            role = f"TOOL:{message.tool_name}"
        content = message.content.strip()
        if not content:
            continue
        parts.append(f"{role}: {content}")
    return "\n\n".join(parts).strip()
