"""platform adapters — bridge Go service McpLiteClients to the MessageBus.

Design
------
Each ``platformAdapter`` wraps an existing ``McpLiteClient`` (owned by
``ServiceManager``) and registers an event handler on it via
``add_event_handler()``.  This keeps a **single socket connection** per
service — no event-stealing between two competing connections.

Inbound path:
    Go service → event frame → McpLiteClient._read_loop
    → on_event() → registered handler → platformAdapter._dispatch()
    → InboundMessage → bus.publish()

Outbound path:
    bus.outbound queue → platformManager._route_outbound()
    → adapter.send(OutboundMessage) → McpLiteClient.request(tool.call)
    → Go service sends reply.
"""

from __future__ import annotations

import asyncio
import json
import logging
from collections.abc import Awaitable, Callable
from typing import Any

from openagent.bus.bus import MessageBus
from openagent.bus.events import InboundMessage, OutboundMessage, SenderInfo
from openagent.observability.logging import get_logger
from openagent.services import protocol as proto

from .mcplite import McpLiteClient

# async fn(platform, platform_id) -> user_key  (e.g. SessionManager.resolve_user_key)
IdentityResolver = Callable[[str, str], Awaitable[str]]

logger = get_logger(__name__)


# ---------------------------------------------------------------------------
# Base
# ---------------------------------------------------------------------------


class PlatformAdapter:
    """Composition wrapper that hooks a McpLiteClient into the MessageBus.

    Subclass and implement ``_to_inbound()`` and ``send()``.  The adapter
    registers itself on the client in ``__init__`` — no further wiring needed.
    """

    def __init__(
        self,
        *,
        platform_name: str,
        client: McpLiteClient,
        bus: MessageBus,
        resolver: IdentityResolver | None = None,
    ) -> None:
        self._platform_name = platform_name
        self._client = client
        self._bus = bus
        self._resolver = resolver
        client.add_event_handler(self._dispatch)

    @property
    def platform_name(self) -> str:
        return self._platform_name

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
        if inbound is None:
            return
        if self._resolver:
            asyncio.ensure_future(self._enrich_and_publish(inbound))
        else:
            asyncio.ensure_future(self._bus.publish(inbound))

    async def _enrich_and_publish(self, inbound: InboundMessage) -> None:
        """Resolve user_key before publishing so the session key is stable."""
        try:
            inbound.sender.user_key = await self._resolver(
                inbound.platform, inbound.sender.user_id
            )
        except Exception:
            logger.warning(
                "Identity resolution failed for %s:%s — falling back to platform:id",
                inbound.platform, inbound.sender.user_id,
            )
        await self._bus.publish(inbound)

    def _on_connection_status(self, data: dict[str, Any]) -> None:
        """Override to cache status fields."""

    def _to_inbound(self, data: dict[str, Any]) -> InboundMessage | None:
        """Map raw event data to an InboundMessage.  Return None to drop."""
        return None

    # ------------------------------------------------------------------
    # Outbound
    # ------------------------------------------------------------------

    async def send(self, msg: OutboundMessage) -> None:
        """Send a reply back through this platform.  Subclasses must override."""
        raise NotImplementedError(f"{type(self).__name__}.send() is not implemented")


# ---------------------------------------------------------------------------
# Discord
# ---------------------------------------------------------------------------


class DiscordPlatformAdapter(PlatformAdapter):
    """Adapter for the Discord Go service.

    Event data fields (from ``discord.message.received``):
        id, platform_id, guild_id, author_id, author, content, is_bot
    """

    def __init__(self, *, client: McpLiteClient, bus: MessageBus, resolver: IdentityResolver | None = None) -> None:
        super().__init__(platform_name="discord", client=client, bus=bus, resolver=resolver)
        self._status: dict[str, Any] = {"connected": False, "authorized": False}
        # stream_key -> message_id for progressive edits (stream_key = f"{platform}:{channel_id}:{session_key}")
        self._stream_message_ids: dict[str, str] = {}

    def _on_connection_status(self, data: dict[str, Any]) -> None:
        self._status.update(data)

    def _to_inbound(self, data: dict[str, Any]) -> InboundMessage | None:
        if data.get("is_bot"):
            return None
        platform_id = str(data.get("platform_id", ""))
        content = str(data.get("content", ""))
        if not platform_id or not content:
            return None
        return InboundMessage(
            platform="discord",
            channel_id=platform_id,
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

    # Discord message limit (API rejects > 2000 chars)
    _MAX_MESSAGE_LEN = 1900
    # Progressive delivery: show first chunk immediately, then edit.
    # Discord allows ~5 edits per 5 seconds — use 1s between edits to stay under limit.
    _PROGRESSIVE_CHUNK = 200
    _PROGRESSIVE_DELAY_S = 1.0

    def _split_content(self, text: str) -> list[str]:
        """Split text into chunks under Discord's limit, preferring newlines."""
        if len(text) <= self._MAX_MESSAGE_LEN:
            return [text] if text else []
        chunks: list[str] = []
        rest = text
        while rest:
            if len(rest) <= self._MAX_MESSAGE_LEN:
                chunks.append(rest)
                break
            cut = rest[: self._MAX_MESSAGE_LEN]
            # Prefer splitting at newline
            last_nl = cut.rfind("\n")
            if last_nl > self._MAX_MESSAGE_LEN // 2:
                cut, rest = cut[: last_nl + 1], rest[last_nl + 1 :]
            else:
                last_space = cut.rfind(" ")
                if last_space > self._MAX_MESSAGE_LEN // 2:
                    cut, rest = cut[: last_space + 1], rest[last_space + 1 :]
                else:
                    rest = rest[self._MAX_MESSAGE_LEN :]
                    cut = cut[: self._MAX_MESSAGE_LEN]
            chunks.append(cut)
        return chunks

    def _parse_message_id(self, result: object) -> str | None:
        """Extract message id from discord.send_message / edit_message JSON result."""
        if result is None:
            return None
        if isinstance(result, str):
            try:
                data = json.loads(result)
            except json.JSONDecodeError:
                return None
        elif isinstance(result, dict):
            data = result
        else:
            return None
        return data.get("id") if isinstance(data, dict) else None

    async def _send_progressive(self, platform_id: str, text: str) -> None:
        """Send content progressively so the user sees it as it arrives."""
        if not text:
            return
        if len(text) <= self._PROGRESSIVE_CHUNK:
            await self._client.request({
                "type": "tool.call",
                "tool": "discord.send_message",
                "params": {"platform_id": platform_id, "text": text},
            })
            return
        # Send first chunk immediately
        current = text[: self._PROGRESSIVE_CHUNK]
        frame = await self._client.request({
            "type": "tool.call",
            "tool": "discord.send_message",
            "params": {"platform_id": platform_id, "text": current},
        })
        msg_id = None
        if hasattr(frame, "result") and frame.result:
            msg_id = self._parse_message_id(frame.result)
        if not msg_id:
            # Fallback: send rest in one go (edit failed to get id)
            rest = text[self._PROGRESSIVE_CHUNK :]
            if rest:
                await self._client.request({
                    "type": "tool.call",
                    "tool": "discord.send_message",
                    "params": {"platform_id": platform_id, "text": rest},
                })
            return
        # Edit progressively (respect Discord ~5 edits/5s rate limit)
        pos = self._PROGRESSIVE_CHUNK
        while pos < len(text):
            await asyncio.sleep(self._PROGRESSIVE_DELAY_S)
            pos = min(pos + self._PROGRESSIVE_CHUNK, len(text))
            current = text[:pos]
            try:
                await self._client.request({
                    "type": "tool.call",
                    "tool": "discord.edit_message",
                    "params": {
                        "platform_id": platform_id,
                        "message_id": msg_id,
                        "text": current,
                    },
                })
            except Exception:
                # Rate limit or edit failed — send remainder as new message
                rest = text[pos:]
                if rest:
                    await self._client.request({
                        "type": "tool.call",
                        "tool": "discord.send_message",
                        "params": {"platform_id": platform_id, "text": rest},
                    })
                break

    def _stream_key(self, msg: OutboundMessage) -> str:
        return f"{msg.platform}:{msg.channel_id}:{msg.session_key}"

    async def send(self, msg: OutboundMessage) -> None:
        content = msg.content or ""
        meta = msg.metadata or {}
        stream_chunk = meta.get("stream_chunk", False)
        stream_end = meta.get("stream_end", False)

        if stream_chunk:
            # True LLM streaming: create or edit message
            key = self._stream_key(msg)
            msg_id = self._stream_message_ids.get(key)
            if msg_id is None:
                # First chunk: create message (content may be partial)
                if not content:
                    return
                frame = await self._client.request({
                    "type": "tool.call",
                    "tool": "discord.send_message",
                    "params": {"platform_id": msg.channel_id, "text": content},
                })
                new_id = self._parse_message_id(
                    getattr(frame, "result", None) if hasattr(frame, "result") else None
                )
                if new_id:
                    self._stream_message_ids[key] = new_id
            else:
                # Subsequent chunk: edit existing message
                if content:
                    try:
                        await self._client.request({
                            "type": "tool.call",
                            "tool": "discord.edit_message",
                            "params": {
                                "platform_id": msg.channel_id,
                                "message_id": msg_id,
                                "text": content,
                            },
                        })
                    except Exception:
                        pass  # Rate limit or edit failed
            if stream_end:
                self._stream_message_ids.pop(key, None)
            return

        # Non-streaming: full message (fallback or when tools were used)
        chunks = self._split_content(content)
        if not chunks:
            return
        if len(chunks) == 1 and len(content) <= self._MAX_MESSAGE_LEN:
            await self._send_progressive(msg.channel_id, content)
        else:
            for chunk in chunks:
                await self._client.request({
                    "type": "tool.call",
                    "tool": "discord.send_message",
                    "params": {"platform_id": msg.channel_id, "text": chunk},
                })


# ---------------------------------------------------------------------------
# Telegram
# ---------------------------------------------------------------------------


class TelegramPlatformAdapter(PlatformAdapter):
    """Adapter for the Telegram Go service.

    Telegram replies require ``user_id`` + ``access_hash`` (the MTProto peer
    identifiers).  These are extracted from the inbound event and stored in
    ``InboundMessage.metadata`` so the agent loop can propagate them to the
    ``OutboundMessage.metadata`` that ``send()`` reads.

    Expected event data fields (from ``telegram.message.received``):
        from_id, access_hash, from_name, username, text, message_id
    """

    def __init__(self, *, client: McpLiteClient, bus: MessageBus, resolver: IdentityResolver | None = None) -> None:
        super().__init__(platform_name="telegram", client=client, bus=bus, resolver=resolver)
        self._status: dict[str, Any] = {"connected": False, "authorized": False}

    def _on_connection_status(self, data: dict[str, Any]) -> None:
        self._status.update(data)

    def _to_inbound(self, data: dict[str, Any]) -> InboundMessage | None:
        from_id = data.get("from_id")
        content = str(data.get("text", ""))
        if not from_id or not content:
            return None
        return InboundMessage(
            platform="telegram",
            channel_id=str(from_id),
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
        user_id = int(msg.channel_id)
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


class SlackPlatformAdapter(PlatformAdapter):
    """Adapter for the Slack Go service.

    Expected event data fields (from ``slack.message.received``):
        platform_id, user_id, username, text, ts, bot_id
    """

    def __init__(self, *, client: McpLiteClient, bus: MessageBus, resolver: IdentityResolver | None = None) -> None:
        super().__init__(platform_name="slack", client=client, bus=bus, resolver=resolver)
        self._status: dict[str, Any] = {"connected": False}

    def _on_connection_status(self, data: dict[str, Any]) -> None:
        self._status.update(data)

    def _to_inbound(self, data: dict[str, Any]) -> InboundMessage | None:
        if data.get("bot_id"):
            return None
        platform_id = str(data.get("platform_id", ""))
        content = str(data.get("text", ""))
        user_id = str(data.get("user_id", ""))
        if not platform_id or not content:
            return None
        return InboundMessage(
            platform="slack",
            channel_id=platform_id,
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
            "params": {"platform_id": msg.channel_id, "text": msg.content},
        })
