"""WhatsApp extension entrypoint module."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from openagent.interfaces import BaseAsyncExtension
from openagent.platforms.whatsapp import WhatsAppTransport

from transports import NeonizeWhatsAppTransport, ServiceWhatsAppTransport


class WhatsAppExtension(BaseAsyncExtension):
    def __init__(
        self,
        *,
        data_dir: str | Path = "data",
        account_id: str = "default",
        backend: str | None = None,
        service_socket_path: str | Path = "data/sockets/whatsapp.sock",
    ):
        resolved_backend = (backend or os.getenv("OPENAGENT_WHATSAPP_BACKEND", "neonize")).strip().lower()
        self._backend = resolved_backend
        self._transport = self._build_transport(
            backend=resolved_backend,
            data_dir=Path(data_dir),
            account_id=account_id,
            service_socket_path=Path(service_socket_path),
        )

    async def initialize(self) -> None:
        await self._transport.start()
        print(f"WhatsApp extension initialized (backend={self._backend}).")

    async def shutdown(self) -> None:
        await self._transport.stop()

    def get_status(self) -> dict[str, Any]:
        return self._transport.get_status()

    def latest_qr(self) -> str | None:
        return self._transport.latest_qr()

    def pop_messages(self) -> list[Any]:
        return self._transport.pop_messages()

    async def send_text(self, channel_id: str, text: str) -> Any:
        return await self._transport.send_text(channel_id, text)

    async def send_image(self, channel_id: str, image_path: str, caption: str | None = None) -> Any:
        sender = getattr(self._transport, "send_image", None)
        if not callable(sender):
            raise RuntimeError(f"send_image is not supported by backend '{self._backend}'.")
        return await sender(channel_id, image_path, caption=caption)

    async def send_document(
        self,
        channel_id: str,
        file_path: str,
        *,
        caption: str | None = None,
        mime_type: str | None = None,
        file_name: str | None = None,
    ) -> Any:
        sender = getattr(self._transport, "send_document", None)
        if not callable(sender):
            raise RuntimeError(f"send_document is not supported by backend '{self._backend}'.")
        return await sender(
            channel_id,
            file_path,
            caption=caption,
            mime_type=mime_type,
            file_name=file_name,
        )

    @staticmethod
    def _build_transport(
        *,
        backend: str,
        data_dir: Path,
        account_id: str,
        service_socket_path: Path,
    ) -> WhatsAppTransport:
        if backend == "neonize":
            return NeonizeWhatsAppTransport(data_dir=data_dir, account_id=account_id)
        if backend == "service":
            return ServiceWhatsAppTransport(socket_path=service_socket_path)
        raise ValueError(f"Unsupported WhatsApp backend: {backend!r}")
