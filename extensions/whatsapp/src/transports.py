"""WhatsApp transport backends: neonize and MCP-lite service."""

from __future__ import annotations

from dataclasses import fields
from pathlib import Path
from threading import Lock
from typing import Any

from openagent.platforms.mcplite import McpLiteClient
from openagent.platforms.whatsapp import WhatsAppTransport
from openagent.services import protocol as proto

from builders import WhatsAppBuilders
from filters import FilterConfig
from gateway import GatewayConfig, WhatsAppGateway
from handlers import WhatsAppHandlers
from heartbeat import HeartbeatTracker
from schema import OpenAgentMessage
from session import SessionConfig, SessionManager


class NeonizeWhatsAppTransport(WhatsAppTransport):
    """Current Python/neonize transport implementation."""

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

    async def start(self) -> None:
        self._gateway.start()

    async def stop(self) -> None:
        self._gateway.stop()

    async def send_text(self, channel_id: str, text: str) -> Any:
        builder = self._resolve_builders()
        return await builder.send_text(channel_id, text)

    def get_status(self) -> dict[str, Any]:
        return self._heartbeat.snapshot().to_dict()

    def latest_qr(self) -> str | None:
        return self._gateway.latest_qr

    def pop_messages(self) -> list[OpenAgentMessage]:
        with self._messages_lock:
            batch = list(self._messages)
            self._messages.clear()
        return batch

    async def send_image(self, channel_id: str, image_path: str, caption: str | None = None) -> Any:
        builder = self._resolve_builders()
        return await builder.send_image(channel_id, image_path, caption=caption)

    async def send_document(
        self,
        channel_id: str,
        file_path: str,
        *,
        caption: str | None = None,
        mime_type: str | None = None,
        file_name: str | None = None,
    ) -> Any:
        builder = self._resolve_builders()
        return await builder.send_document(
            channel_id,
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


class ServiceWhatsAppTransport(McpLiteClient, WhatsAppTransport):
    """MCP-lite transport scaffold for services/whatsapp."""

    def __init__(self, *, socket_path: str | Path = "data/sockets/whatsapp.sock"):
        super().__init__(socket_path=socket_path)
        self._status: dict[str, Any] = {
            "running": False,
            "connected": False,
            "backend": "service",
            "service_socket": str(socket_path),
        }
        self._latest_qr: str | None = None
        self._messages: list[OpenAgentMessage] = []

    async def start(self) -> None:
        await super().start()
        self._status["running"] = True

    async def stop(self) -> None:
        await super().stop()
        self._status["running"] = False
        self._status["connected"] = False

    async def send_text(self, channel_id: str, text: str) -> Any:
        frame = await self.request(
            {
                "type": "tool.call",
                "tool": "whatsapp.send_text",
                "params": {"chat_id": channel_id, "text": text},
            }
        )
        if not isinstance(frame, proto.ToolResultResponse):
            raise RuntimeError(f"unexpected response type: {type(frame).__name__}")
        if frame.error:
            raise RuntimeError(frame.error)
        return frame.result

    def get_status(self) -> dict[str, Any]:
        return dict(self._status)

    def latest_qr(self) -> str | None:
        return self._latest_qr

    def pop_messages(self) -> list[OpenAgentMessage]:
        batch = list(self._messages)
        self._messages.clear()
        return batch

    def on_event(self, frame: proto.EventFrame) -> None:
        if frame.event == "whatsapp.qr":
            self._latest_qr = str(frame.data.get("qr") or "")
            return
        if frame.event == "whatsapp.connection.status":
            if "connected" in frame.data:
                self._status["connected"] = bool(frame.data.get("connected"))
            for key, value in frame.data.items():
                self._status[key] = value
            return
        if frame.event == "whatsapp.message.received":
            self._messages.append(_coerce_message(frame.data))


def _coerce_message(data: dict[str, Any]) -> OpenAgentMessage:
    valid_fields = {item.name for item in fields(OpenAgentMessage)}
    payload = {key: value for key, value in data.items() if key in valid_fields}
    payload.setdefault("raw_event", data)
    return OpenAgentMessage(**payload)
