"""Neonize event handlers for WhatsApp gateway."""

from __future__ import annotations

from typing import Any, Awaitable, Callable

from filters import FilterConfig, should_process_message
from heartbeat import HeartbeatTracker
from schema import OpenAgentMessage, from_neonize_message_event


class WhatsAppHandlers:
    def __init__(
        self,
        *,
        heartbeat: HeartbeatTracker,
        filter_config: FilterConfig,
        account_id: str = "default",
        self_id_getter: Callable[[], str | None] | None = None,
        on_message: Callable[[OpenAgentMessage], Awaitable[None] | None] | None = None,
    ):
        self._heartbeat = heartbeat
        self._filter_config = filter_config
        self._account_id = account_id
        self._self_id_getter = self_id_getter
        self._on_message = on_message

    async def handle_event(self, event: Any) -> OpenAgentMessage | None:
        event_type = self._event_type(event)
        lowered = event_type.lower()
        if "message" in lowered:
            return await self.handle_message(event)
        if "connect" in lowered or "disconnect" in lowered:
            await self.handle_connection(event)
            return None
        self._heartbeat.mark_event()
        return None

    async def handle_message(self, event: Any) -> OpenAgentMessage | None:
        message = from_neonize_message_event(event, account_id=self._account_id)
        if message is None:
            return None
        self._heartbeat.mark_event()

        self_id = self._self_id_getter() if self._self_id_getter else None
        allowed, _reason = should_process_message(
            message,
            self._filter_config,
            self_id=self_id,
        )
        if not allowed:
            return None

        self._heartbeat.mark_message()
        if self._on_message:
            maybe = self._on_message(message)
            if maybe is not None:
                await maybe
        return message

    async def handle_connection(self, event: Any) -> None:
        status = str(self._read(event, "status", "state", "event", "type") or "").lower()
        code = self._read(event, "code", "status_code")
        reason = self._read(event, "reason", "error")
        if "connect" in status and "disconnect" not in status:
            self._heartbeat.mark_connected(self_id=self._read(event, "self_id", "jid"))
            return
        self._heartbeat.mark_disconnected(reason=str(reason) if reason else None, code=code)
        if reason:
            self._heartbeat.mark_error(str(reason))

    @staticmethod
    def _event_type(event: Any) -> str:
        return str(
            WhatsAppHandlers._read(event, "type", "event_type", "event", "name")
            or event.__class__.__name__
        )

    @staticmethod
    def _read(source: Any, *keys: str) -> Any:
        for key in keys:
            if isinstance(source, dict) and key in source:
                return source[key]
            if hasattr(source, key):
                return getattr(source, key)
        return None
