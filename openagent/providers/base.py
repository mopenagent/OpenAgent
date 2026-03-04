"""Provider base types — Message dataclass + Provider Protocol."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import AsyncIterator, Literal, Protocol, runtime_checkable


@dataclass
class Message:
    role: Literal["system", "user", "assistant"]
    content: str


@runtime_checkable
class Provider(Protocol):
    """Streaming-first LLM provider interface."""

    async def stream(
        self, messages: list[Message], **kwargs
    ) -> AsyncIterator[str]:
        """Yield text chunks as they arrive from the model."""
        ...

    async def complete(self, messages: list[Message], **kwargs) -> str:
        """Return the full response (collects the stream)."""
        ...
