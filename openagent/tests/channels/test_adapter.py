"""Tests for ChannelAdapter and ChannelManager."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import pytest
import pytest_asyncio

from openagent.bus.bus import MessageBus
from openagent.bus.events import InboundMessage, OutboundMessage
from openagent.channels.adapter import (
    ChannelAdapter,
    DiscordChannelAdapter,
    SlackChannelAdapter,
    TelegramChannelAdapter,
)
from openagent.channels.manager import ChannelManager
from openagent.channels.mcplite import McpLiteClient
from openagent.services import protocol as proto


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_event(event_name: str, data: dict[str, Any]) -> proto.EventFrame:
    return proto.EventFrame(type="event", event=event_name, data=data)


def _make_mock_client() -> MagicMock:
    client = MagicMock(spec=McpLiteClient)
    client._event_handlers = []

    def _add_handler(h):
        client._event_handlers.append(h)

    client.add_event_handler.side_effect = _add_handler
    client.request = AsyncMock(return_value=MagicMock())
    return client


def _fire_event(client: MagicMock, frame: proto.EventFrame) -> None:
    for h in client._event_handlers:
        h(frame)


# ---------------------------------------------------------------------------
# DiscordChannelAdapter
# ---------------------------------------------------------------------------


class TestDiscordChannelAdapter:
    def setup_method(self):
        self.client = _make_mock_client()
        self.bus = MagicMock(spec=MessageBus)
        self.bus.publish = AsyncMock()
        self.adapter = DiscordChannelAdapter(client=self.client, bus=self.bus)

    def test_registers_event_handler(self):
        assert len(self.client._event_handlers) == 1

    def test_channel_name(self):
        assert self.adapter.channel_name == "discord"

    def test_client_identity(self):
        assert self.adapter.client is self.client

    def test_to_inbound_basic(self):
        msg = self.adapter._to_inbound({
            "id": "m1",
            "channel_id": "chan1",
            "guild_id": "guild1",
            "author_id": "user1",
            "author": "Alice",
            "content": "hello",
            "is_bot": False,
        })
        assert msg is not None
        assert msg.channel == "discord"
        assert msg.chat_id == "chan1"
        assert msg.content == "hello"
        assert msg.sender.user_id == "user1"
        assert msg.sender.display_name == "Alice"
        assert msg.metadata["guild_id"] == "guild1"

    def test_to_inbound_bot_filtered(self):
        assert self.adapter._to_inbound({"is_bot": True, "content": "hi"}) is None

    def test_to_inbound_missing_channel_id(self):
        assert self.adapter._to_inbound({"content": "hi"}) is None

    def test_to_inbound_empty_content(self):
        assert self.adapter._to_inbound({"channel_id": "c1", "content": ""}) is None

    def test_connection_status_updates_status(self):
        _fire_event(
            self.client,
            _make_event("discord.connection.status", {"connected": True, "authorized": True}),
        )
        assert self.adapter._status["connected"] is True

    def test_message_event_schedules_publish(self):
        loop = asyncio.new_event_loop()
        try:
            published: list[InboundMessage] = []

            async def fake_publish(msg):
                published.append(msg)

            self.bus.publish = fake_publish

            async def run():
                _fire_event(
                    self.client,
                    _make_event("discord.message.received", {
                        "id": "m2",
                        "channel_id": "chan2",
                        "author_id": "u2",
                        "author": "Bob",
                        "content": "hey",
                        "is_bot": False,
                    }),
                )
                await asyncio.sleep(0)  # yield to let ensure_future run

            loop.run_until_complete(run())
        finally:
            loop.close()

        assert len(published) == 1
        assert published[0].content == "hey"
        assert published[0].chat_id == "chan2"

    def test_send_calls_tool(self):
        async def run():
            msg = OutboundMessage(channel="discord", chat_id="chan3", content="reply")
            await self.adapter.send(msg)

        asyncio.run(run())
        self.client.request.assert_called_once()
        call_args = self.client.request.call_args[0][0]
        assert call_args["tool"] == "discord.send_message"
        assert call_args["params"]["channel_id"] == "chan3"
        assert call_args["params"]["text"] == "reply"


# ---------------------------------------------------------------------------
# TelegramChannelAdapter
# ---------------------------------------------------------------------------


class TestTelegramChannelAdapter:
    def setup_method(self):
        self.client = _make_mock_client()
        self.bus = MagicMock(spec=MessageBus)
        self.bus.publish = AsyncMock()
        self.adapter = TelegramChannelAdapter(client=self.client, bus=self.bus)

    def test_to_inbound_basic(self):
        msg = self.adapter._to_inbound({
            "from_id": 12345,
            "access_hash": -9999,
            "from_name": "Charlie",
            "username": "charlie",
            "text": "hello tg",
            "message_id": 77,
        })
        assert msg is not None
        assert msg.channel == "telegram"
        assert msg.chat_id == "12345"
        assert msg.content == "hello tg"
        assert msg.metadata["access_hash"] == -9999
        assert msg.metadata["message_id"] == 77

    def test_to_inbound_missing_from_id(self):
        assert self.adapter._to_inbound({"text": "hi"}) is None

    def test_to_inbound_empty_text(self):
        assert self.adapter._to_inbound({"from_id": 1, "text": ""}) is None

    def test_send_calls_tool_with_access_hash(self):
        async def run():
            msg = OutboundMessage(
                channel="telegram",
                chat_id="12345",
                content="tg reply",
                metadata={"access_hash": -9999},
            )
            await self.adapter.send(msg)

        asyncio.run(run())
        self.client.request.assert_called_once()
        params = self.client.request.call_args[0][0]["params"]
        assert params["user_id"] == 12345
        assert params["access_hash"] == -9999
        assert params["text"] == "tg reply"


# ---------------------------------------------------------------------------
# SlackChannelAdapter
# ---------------------------------------------------------------------------


class TestSlackChannelAdapter:
    def setup_method(self):
        self.client = _make_mock_client()
        self.bus = MagicMock(spec=MessageBus)
        self.adapter = SlackChannelAdapter(client=self.client, bus=self.bus)

    def test_to_inbound_basic(self):
        msg = self.adapter._to_inbound({
            "channel_id": "C123",
            "user_id": "U456",
            "username": "dave",
            "text": "slack msg",
            "ts": "1234.5678",
        })
        assert msg is not None
        assert msg.channel == "slack"
        assert msg.chat_id == "C123"
        assert msg.content == "slack msg"

    def test_bot_message_filtered(self):
        assert self.adapter._to_inbound({"bot_id": "B123", "channel_id": "C1", "text": "bot"}) is None

    def test_send_calls_tool(self):
        async def run():
            await self.adapter.send(OutboundMessage(channel="slack", chat_id="C999", content="yo"))

        asyncio.run(run())
        params = self.client.request.call_args[0][0]["params"]
        assert params["channel_id"] == "C999"
        assert params["text"] == "yo"


# ---------------------------------------------------------------------------
# ChannelManager
# ---------------------------------------------------------------------------


class TestChannelManager:
    def _make_service_manager(self, clients: dict[str, Any]) -> MagicMock:
        svc_mgr = MagicMock()
        svc_mgr.get_client.side_effect = lambda name: clients.get(name)
        return svc_mgr

    def test_sync_adapters_creates_adapter_for_online_service(self):
        client = _make_mock_client()
        client.running = True
        svc_mgr = self._make_service_manager({"discord": client})
        bus = MagicMock(spec=MessageBus)
        mgr = ChannelManager(service_manager=svc_mgr, bus=bus)
        mgr._sync_adapters()
        assert "discord" in mgr._adapters
        assert isinstance(mgr._adapters["discord"], DiscordChannelAdapter)

    def test_sync_adapters_removes_adapter_when_service_offline(self):
        client = _make_mock_client()
        svc_mgr = self._make_service_manager({"discord": client})
        bus = MagicMock(spec=MessageBus)
        mgr = ChannelManager(service_manager=svc_mgr, bus=bus)
        mgr._sync_adapters()
        assert "discord" in mgr._adapters
        # Service goes offline
        svc_mgr.get_client.side_effect = lambda name: None
        mgr._sync_adapters()
        assert "discord" not in mgr._adapters

    def test_sync_adapters_rebuilds_on_new_client(self):
        client1 = _make_mock_client()
        client2 = _make_mock_client()
        svc_mgr = self._make_service_manager({"discord": client1})
        bus = MagicMock(spec=MessageBus)
        mgr = ChannelManager(service_manager=svc_mgr, bus=bus)
        mgr._sync_adapters()
        adapter1 = mgr._adapters["discord"]
        # Service restarts with new client
        svc_mgr.get_client.side_effect = lambda name: {"discord": client2}.get(name)
        mgr._sync_adapters()
        adapter2 = mgr._adapters["discord"]
        assert adapter2 is not adapter1
        assert adapter2.client is client2

    def test_same_client_no_rebuild(self):
        client = _make_mock_client()
        svc_mgr = self._make_service_manager({"discord": client})
        bus = MagicMock(spec=MessageBus)
        mgr = ChannelManager(service_manager=svc_mgr, bus=bus)
        mgr._sync_adapters()
        adapter1 = mgr._adapters["discord"]
        mgr._sync_adapters()
        assert mgr._adapters["discord"] is adapter1  # not rebuilt

    def test_route_outbound_dispatches_to_adapter(self):
        async def run():
            bus = MessageBus()
            await bus.start()

            client = _make_mock_client()
            svc_mgr = self._make_service_manager({"discord": client})
            mgr = ChannelManager(service_manager=svc_mgr, bus=bus)
            mgr._sync_adapters()

            # Track calls to adapter.send
            sent: list[OutboundMessage] = []
            mgr._adapters["discord"].send = AsyncMock(side_effect=sent.append)

            await mgr.start()
            await bus.dispatch(OutboundMessage(channel="discord", chat_id="C1", content="hi"))
            await asyncio.sleep(0.05)  # let route task process
            await mgr.stop()
            await bus.close()

            return sent

        sent = asyncio.run(run())
        assert len(sent) == 1
        assert sent[0].content == "hi"

    def test_route_outbound_logs_unknown_channel(self):
        async def run():
            bus = MessageBus()
            await bus.start()
            svc_mgr = self._make_service_manager({})
            mgr = ChannelManager(service_manager=svc_mgr, bus=bus)
            await mgr.start()
            # Dispatch to unknown channel
            await bus.dispatch(OutboundMessage(channel="unknown", chat_id="x", content="?"))
            await asyncio.sleep(0.05)
            await mgr.stop()
            await bus.close()

        asyncio.run(run())  # Should not raise

    def test_manual_register_overrides_auto(self):
        client1 = _make_mock_client()
        client2 = _make_mock_client()
        svc_mgr = self._make_service_manager({"discord": client1})
        bus = MagicMock(spec=MessageBus)
        mgr = ChannelManager(service_manager=svc_mgr, bus=bus)
        custom_adapter = DiscordChannelAdapter(client=client2, bus=bus)
        mgr.register(custom_adapter)
        assert mgr._adapters["discord"] is custom_adapter


# ---------------------------------------------------------------------------
# Identity resolver
# ---------------------------------------------------------------------------


class TestIdentityResolver:
    """ChannelAdapter enriches sender.user_key when a resolver is provided."""

    def setup_method(self):
        self.client = _make_mock_client()
        self.bus = MagicMock(spec=MessageBus)
        self.bus.publish = AsyncMock()

    @pytest.mark.asyncio
    async def test_resolver_sets_user_key(self):
        """When a resolver is wired, the adapter enriches user_key before publish."""
        async def fake_resolver(channel: str, channel_id: str) -> str:
            return f"user:resolved-{channel_id}"

        adapter = DiscordChannelAdapter(
            client=self.client, bus=self.bus, resolver=fake_resolver
        )
        event = _make_event("discord.message.received", {
            "channel_id": "C1",
            "author_id": "U1",
            "author": "alice",
            "content": "hello",
            "is_bot": False,
        })
        _fire_event(self.client, event)
        await asyncio.sleep(0)  # let ensure_future run

        assert self.bus.publish.called
        msg: InboundMessage = self.bus.publish.call_args[0][0]
        assert msg.sender.user_key == "user:resolved-U1"
        # session_key now uses the resolved user_key
        assert msg.session_key == "user:resolved-U1"

    @pytest.mark.asyncio
    async def test_no_resolver_user_key_empty(self):
        """Without a resolver the adapter publishes with user_key == '' (fallback key)."""
        adapter = DiscordChannelAdapter(client=self.client, bus=self.bus)
        event = _make_event("discord.message.received", {
            "channel_id": "C2",
            "author_id": "U2",
            "author": "bob",
            "content": "hi",
            "is_bot": False,
        })
        _fire_event(self.client, event)
        await asyncio.sleep(0)

        msg: InboundMessage = self.bus.publish.call_args[0][0]
        assert msg.sender.user_key == ""
        # Falls back to channel:chat_id
        assert msg.session_key == "discord:C2"

    @pytest.mark.asyncio
    async def test_resolver_failure_falls_back_gracefully(self):
        """If the resolver raises, the message is still published (no user_key)."""
        async def bad_resolver(channel: str, channel_id: str) -> str:
            raise RuntimeError("db is down")

        adapter = DiscordChannelAdapter(
            client=self.client, bus=self.bus, resolver=bad_resolver
        )
        event = _make_event("discord.message.received", {
            "channel_id": "C3",
            "author_id": "U3",
            "author": "carol",
            "content": "test",
            "is_bot": False,
        })
        _fire_event(self.client, event)
        await asyncio.sleep(0)

        # Message still reaches the bus despite resolver failure
        assert self.bus.publish.called
        msg: InboundMessage = self.bus.publish.call_args[0][0]
        assert msg.sender.user_key == ""
