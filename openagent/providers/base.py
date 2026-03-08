"""Provider base types — Message, ToolCall, LLMResponse, Provider Protocol."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, AsyncIterator, Literal, Protocol, runtime_checkable


# ---------------------------------------------------------------------------
# Stream events (Path A: delta-based streaming with tools)
# ---------------------------------------------------------------------------


@dataclass
class StreamEvent:
    """Single event from stream_with_tools.

    - content: text delta — stream immediately to UI
    - tool_calls: when stream ends with tool_calls, execute and continue loop
    - finish_reason: stop | tool_calls | length | content_filter
    """

    content: str | None = None
    tool_calls: list["ToolCall"] | None = None
    finish_reason: str | None = None


@dataclass
class Message:
    role: Literal["system", "user", "assistant", "tool"]
    content: str
    tool_call_id: str = ""   # set when role == "tool" (result injection)
    tool_name: str = ""      # set when role == "tool"
    tool_calls: list["ToolCall"] | None = None  # set when role == "assistant" and LLM called tools


@dataclass
class ToolCall:
    """A single tool invocation requested by the LLM."""
    id: str
    name: str
    arguments: dict[str, Any]


@dataclass
class LLMResponse:
    """Full response from a chat() call — text and/or tool calls."""
    content: str
    tool_calls: list[ToolCall] = field(default_factory=list)

    @property
    def has_tool_calls(self) -> bool:
        return bool(self.tool_calls)


@runtime_checkable
class Provider(Protocol):
    """LLM provider interface — streaming text and agentic tool-calling."""

    async def stream(
        self, messages: list[Message], **kwargs
    ) -> AsyncIterator[str]:
        """Yield text chunks as they arrive from the model."""
        ...

    async def complete(self, messages: list[Message], **kwargs) -> str:
        """Return the full text response (no tool calling)."""
        ...

    async def chat(
        self,
        messages: list[Message],
        tools: list[dict[str, Any]] | None = None,
        **kwargs,
    ) -> LLMResponse:
        """Send a chat request with optional tool schemas.

        Returns the text content and any tool_calls the model emitted.
        When ``tools`` is None or empty the model will not use tools.
        """
        ...

    async def stream_with_tools(
        self,
        messages: list[Message],
        *,
        tools: list[dict[str, Any]] | None = None,
        **kwargs,
    ) -> AsyncIterator[StreamEvent]:
        """Stream a chat request, yielding content deltas and final tool_calls.

        Implementations that genuinely cannot stream may fall back to wrapping
        ``chat()`` — yielding one ``StreamEvent(content=...)`` then optionally
        one ``StreamEvent(tool_calls=...)``.  The agent loop treats the
        interface uniformly regardless of whether real streaming occurs.
        """
        ...
