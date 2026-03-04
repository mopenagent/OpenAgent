"""WhatsApp backend transport contract."""

from __future__ import annotations

from abc import ABC, abstractmethod
from typing import Any


class WhatsAppTransport(ABC):
    """Backend-agnostic transport API for WhatsApp integrations."""

    @abstractmethod
    async def start(self) -> None:
        """Start transport resources and background workers."""

    @abstractmethod
    async def stop(self) -> None:
        """Stop transport resources and background workers."""

    @abstractmethod
    async def send_text(self, chat_id: str, text: str) -> Any:
        """Send a plain text message."""

    @abstractmethod
    def get_status(self) -> dict[str, Any]:
        """Return transport health and connection state."""

    @abstractmethod
    def latest_qr(self) -> str | None:
        """Return latest QR payload if available."""

    @abstractmethod
    def pop_messages(self) -> list[Any]:
        """Drain inbound messages captured by the transport."""
