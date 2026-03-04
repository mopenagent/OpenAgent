"""Channel adapters — bridge Go service McpLiteClients to the MessageBus.

Design
------
Each ``ChannelAdapter`` wraps an existing ``McpLiteClient`` (owned by
``ServiceManager``) and registers an event handler on it via
``add_event_handler()``.  This keeps a **single socket connection** per
service — no event-stealing between two competing connections.

Inbound path:
    Go service → event frame → McpLiteClient._read_loop
    → on_event() → registered handler → ChannelAdapter._dispatch()
    → InboundMessage → bus.publish()

Outbound path:
    bus.outbound queue → ChannelManager._route_outbound()
    → adapter.send(OutboundMessage) → McpLiteClient.request(tool.call)
    → Go service sends reply.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from openagent.bus.bus import MessageBus
from openagent.bus.events import InboundMessage, OutboundMessage, SenderInfo
from openagent.observability.logging import get_logger
from openagent.services import protocol as proto

from .mcplite import McpLiteClient

logger = get_logger(__name__)


# ---------------------------------------------------------------------------
# Base
# ---------------------------------------------------------------------------


class ChannelAdapter:
    """Composition wrapper that hooks a McpLiteClient into the MessageBus.

    Subclass and implement ``_to_inbound()`` and ``send()``.  The adapter
    registers itself on the client in ``__init__`` — no further wiring needed.
    """

    def __init__(
        self,
        *,
        channel_name: str,
        client: McpLiteClient,
        bus: MessageBus,
    ) -> None:
        self._channel_name = channel_name
        self._client = client
        self._bus = bus
        client.add_event_handler(self._dispatch)

    @property
    def channel_name(self) -> str:
        return self._channel_name

    @property
    def client(self) -> McpLiteClient:
        """The underlying client — used for identity checks on restart."""
        return self._client

    # ------------------------------------------------------------------
    # Event dispatch (sync — called from async _read_loop via on_event)
    # ------------------------------------------------------------------

    def _dispatch(self, frame: proto.EventFrame) -> None:
        data = dict(frame.data)
        if "connection.status" in frame.event:
            self._on_connection_status(data)
            return
        inbound = self._to_inbound(data)
        if inbound is not None:
            asyncio.ensure_future(self._bus.publish(inbound))

    def _on_connection_status(self, data: dict[str, Any]) -> None:
        """Override to cache status fields."""

    def _to_inbound(self, data: dict[str, Any]) -> InboundMessage | None:
        """Map raw event data to an InboundMessage.  Return None to drop."""
        return None

    # ------------------------------------------------------------------
    # Outbound
    # ------------------------------------------------------------------

    async def send(self, msg: OutboundMessage) -> None:
        """Send a reply back through this channel.  Subclasses must override."""
        raise NotImplementedError(f"{type(self).__name__}.send() is not implemented")


# ---------------------------------------------------------------------------
# Discord
# ---------------------------------------------------------------------------


class DiscordChannelAdapter(ChannelAdapter):
    """Adapter for the Discord Go service.

    Event data fields (from ``discord.message.received``):
        id, channel_id, guild_id, author_id, author, content, is_bot
    """

    def __init__(self, *, client: McpLiteClient, bus: MessageBus) -> None:
        super().__init__(channel_name="discord", client=client, bus=bus)
        self._status: dict[str, Any] = {"connected": False, "authorized": False}

    def _on_connection_status(self, data: dict[str, Any]) -> None:
        self._status.update(data)

    def _to_inbound(self, data: dict[str, Any]) -> InboundMessage | None:
        if data.get("is_bot"):
            return None
        channel_id = str(data.get("channel_id", ""))
        content = str(data.get("content", ""))
        if not channel_id or not content:
            return None
        return InboundMessage(
            channel="discord",
            chat_id=channel_id,
            sender=SenderInfo(
                platform="discord",
                user_id=str(data.get("author_id", "")),
                display_name=str(data.get("author", "")),
            ),
            content=content,
            metadata={
                "message_id": data.get("id", ""),
                "guild_id": data.get("guild_id", ""),
            },
        )

    async def send(self, msg: OutboundMessage) -> None:
        await self._client.request({
            "type": "tool.call",
            "tool": "discord.send_message",
            "params": {"channel_id": msg.chat_id, "text": msg.content},
        })


# ---------------------------------------------------------------------------
# Telegram
# ---------------------------------------------------------------------------


class TelegramChannelAdapter(ChannelAdapter):
    """Adapter for the Telegram Go service.

    Telegram replies require ``user_id`` + ``access_hash`` (the MTProto peer
    identifiers).  These are extracted from the inbound event and stored in
    ``InboundMessage.metadata`` so the agent loop can propagate them to the
    ``OutboundMessage.metadata`` that ``send()`` reads.

    Expected event data fields (from ``telegram.message.received``):
        from_id, access_hash, from_name, username, text, message_id
    """

    def __init__(self, *, client: McpLiteClient, bus: MessageBus) -> None:
        super().__init__(channel_name="telegram", client=client, bus=bus)
        self._status: dict[str, Any] = {"connected": False, "authorized": False}

    def _on_connection_status(self, data: dict[str, Any]) -> None:
        self._status.update(data)

    def _to_inbound(self, data: dict[str, Any]) -> InboundMessage | None:
        from_id = data.get("from_id")
        content = str(data.get("text", ""))
        if not from_id or not content:
            return None
        return InboundMessage(
            channel="telegram",
            chat_id=str(from_id),
            sender=SenderInfo(
                platform="telegram",
                user_id=str(from_id),
                display_name=str(data.get("from_name", "")),
            ),
            content=content,
            metadata={
                "access_hash": data.get("access_hash", 0),
                "message_id": data.get("message_id", 0),
                "username": data.get("username", ""),
            },
        )

    async def send(self, msg: OutboundMessage) -> None:
        user_id = int(msg.chat_id)
        access_hash = int(msg.metadata.get("access_hash", 0))
        await self._client.request({
            "type": "tool.call",
            "tool": "telegram.send_message",
            "params": {
                "user_id": user_id,
                "access_hash": access_hash,
                "text": msg.content,
            },
        })


# ---------------------------------------------------------------------------
# Slack
# ---------------------------------------------------------------------------


class SlackChannelAdapter(ChannelAdapter):
    """Adapter for the Slack Go service.

    Expected event data fields (from ``slack.message.received``):
        channel_id, user_id, username, text, ts, bot_id
    """

    def __init__(self, *, client: McpLiteClient, bus: MessageBus) -> None:
        super().__init__(channel_name="slack", client=client, bus=bus)
        self._status: dict[str, Any] = {"connected": False}

    def _on_connection_status(self, data: dict[str, Any]) -> None:
        self._status.update(data)

    def _to_inbound(self, data: dict[str, Any]) -> InboundMessage | None:
        if data.get("bot_id"):
            return None
        channel_id = str(data.get("channel_id", ""))
        content = str(data.get("text", ""))
        user_id = str(data.get("user_id", ""))
        if not channel_id or not content:
            return None
        return InboundMessage(
            channel="slack",
            chat_id=channel_id,
            sender=SenderInfo(
                platform="slack",
                user_id=user_id,
                display_name=str(data.get("username", "")),
            ),
            content=content,
            metadata={"message_ts": data.get("ts", "")},
        )

    async def send(self, msg: OutboundMessage) -> None:
        await self._client.request({
            "type": "tool.call",
            "tool": "slack.send_message",
            "params": {"channel_id": msg.chat_id, "text": msg.content},
        })
