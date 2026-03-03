"""Shared interfaces expected by extensions."""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Any, Protocol, runtime_checkable


@runtime_checkable
class AsyncExtension(Protocol):
    """Strict async extension contract consumed by the core manager."""

    async def initialize(self) -> None:
        """Run extension startup logic."""

    async def shutdown(self) -> None:
        """Clean up extension resources."""

    def get_status(self) -> dict[str, Any]:
        """Expose extension health/status information."""


class BaseAsyncExtension(ABC):
    """Runtime base class for first-class extensions."""

    @abstractmethod
    async def initialize(self) -> None:
        """Run extension startup logic."""

    async def shutdown(self) -> None:
        """Clean up extension resources."""

    def get_status(self) -> dict[str, Any]:
        """Expose extension health/status information."""
        return {}
