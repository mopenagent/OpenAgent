"""WhatsApp extension entrypoint module."""

from __future__ import annotations

from pathlib import Path
from threading import Lock
from typing import Any

from openagent.interfaces import BaseAsyncExtension

from builders import WhatsAppBuilders
from filters import FilterConfig
from gateway import GatewayConfig, WhatsAppGateway
from handlers import WhatsAppHandlers
from heartbeat import HeartbeatTracker
from schema import OpenAgentMessage
from session import SessionConfig, SessionManager


class WhatsAppExtension(BaseAsyncExtension):
    def __init__(
        self,
        *,
        data_dir: str | Path = "data",
        account_id: str = "default",
    ):
        self._messages: list[OpenAgentMessage] = []
        self._messages_lock = Lock()
        self._heartbeat = HeartbeatTracker()
        self._session = SessionManager(
            SessionConfig(data_dir=Path(data_dir), account_id=account_id)
        )
        self._handlers = WhatsAppHandlers(
            heartbeat=self._heartbeat,
            filter_config=FilterConfig(),
            account_id=account_id,
            self_id_getter=self._session.read_self_id,
            on_message=self._capture_message,
        )
        self._gateway = WhatsAppGateway(
            session=self._session,
            handlers=self._handlers,
            heartbeat=self._heartbeat,
            config=GatewayConfig(),
        )
        self._builders: WhatsAppBuilders | None = None

    async def initialize(self) -> None:
        self._gateway.start()
        print("WhatsApp extension initialized.")

    async def shutdown(self) -> None:
        self._gateway.stop()

    def get_status(self) -> dict[str, Any]:
        return self._heartbeat.snapshot().to_dict()

    def latest_qr(self) -> str | None:
        return self._gateway.latest_qr

    def pop_messages(self) -> list[OpenAgentMessage]:
        with self._messages_lock:
            batch = list(self._messages)
            self._messages.clear()
        return batch

    async def send_text(self, chat_id: str, text: str) -> Any:
        builder = self._resolve_builders()
        return await builder.send_text(chat_id, text)

    async def send_image(self, chat_id: str, image_path: str, caption: str | None = None) -> Any:
        builder = self._resolve_builders()
        return await builder.send_image(chat_id, image_path, caption=caption)

    async def send_document(
        self,
        chat_id: str,
        file_path: str,
        *,
        caption: str | None = None,
        mime_type: str | None = None,
        file_name: str | None = None,
    ) -> Any:
        builder = self._resolve_builders()
        return await builder.send_document(
            chat_id,
            file_path,
            caption=caption,
            mime_type=mime_type,
            file_name=file_name,
        )

    async def _capture_message(self, message: OpenAgentMessage) -> None:
        with self._messages_lock:
            self._messages.append(message)

    def _resolve_builders(self) -> WhatsAppBuilders:
        if self._builders:
            return self._builders
        client = self._gateway.get_client()
        if not client:
            raise RuntimeError("WhatsApp gateway is not connected.")
        self._builders = WhatsAppBuilders(client)
        return self._builders
