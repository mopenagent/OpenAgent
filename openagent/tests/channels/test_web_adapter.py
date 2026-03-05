"""Tests for WebChannelAdapter."""

from __future__ import annotations

import asyncio
from unittest.mock import AsyncMock

import pytest

from openagent.bus.events import OutboundMessage
from openagent.channels.web import WebChannelAdapter


class TestWebChannelAdapter:
    def setup_method(self):
        self.adapter = WebChannelAdapter()

    def test_channel_name(self):
        assert self.adapter.channel_name == "web"

    def test_register_connection(self):
        send_fn = AsyncMock()
        self.adapter.register_connection("sess1", send_fn)
        assert "sess1" in self.adapter.active_connections()

    def test_unregister_connection(self):
        send_fn = AsyncMock()
        self.adapter.register_connection("sess1", send_fn)
        self.adapter.unregister_connection("sess1")
        assert "sess1" not in self.adapter.active_connections()

    def test_unregister_nonexistent_is_safe(self):
        self.adapter.unregister_connection("unknown")  # should not raise

    def test_send_calls_registered_fn(self):
        delivered: list[str] = []

        async def send_fn(content: str) -> None:
            delivered.append(content)

        self.adapter.register_connection("sess1", send_fn)
        asyncio.run(
            self.adapter.send(OutboundMessage(channel="web", chat_id="sess1", content="hello"))
        )
        assert delivered == ["hello"]

    def test_send_unknown_chat_id_is_safe(self):
        # No connection registered — should log warning but not raise.
        asyncio.run(
            self.adapter.send(OutboundMessage(channel="web", chat_id="ghost", content="hi"))
        )

    def test_send_removes_dead_connection_on_error(self):
        async def failing_fn(content: str) -> None:
            raise RuntimeError("WS closed")

        self.adapter.register_connection("sess1", failing_fn)
        asyncio.run(
            self.adapter.send(OutboundMessage(channel="web", chat_id="sess1", content="boom"))
        )
        # Dead connection should have been cleaned up.
        assert "sess1" not in self.adapter.active_connections()

    def test_multiple_connections_independent(self):
        received: dict[str, list[str]] = {"a": [], "b": []}

        async def fn_a(c): received["a"].append(c)
        async def fn_b(c): received["b"].append(c)

        self.adapter.register_connection("a", fn_a)
        self.adapter.register_connection("b", fn_b)

        asyncio.run(self.adapter.send(OutboundMessage(channel="web", chat_id="a", content="for-a")))
        asyncio.run(self.adapter.send(OutboundMessage(channel="web", chat_id="b", content="for-b")))

        assert received["a"] == ["for-a"]
        assert received["b"] == ["for-b"]

    def test_active_connections_empty_initially(self):
        assert self.adapter.active_connections() == []

    def test_active_connections_reflects_registrations(self):
        self.adapter.register_connection("x", AsyncMock())
        self.adapter.register_connection("y", AsyncMock())
        assert set(self.adapter.active_connections()) == {"x", "y"}
