"""Tests for openagent.bus — MessageBus session-aware routing."""

from __future__ import annotations

import asyncio

import pytest

from openagent.bus import InboundMessage, MessageBus, OutboundMessage, SenderInfo


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _msg(
    channel: str = "telegram",
    user_id: str = "u1",
    chat_id: str = "c1",
    content: str = "hello",
    canonical_id: str = "",
    session_key_override: str | None = None,
) -> InboundMessage:
    return InboundMessage(
        channel=channel,
        sender=SenderInfo(platform=channel, user_id=user_id, canonical_id=canonical_id),
        chat_id=chat_id,
        content=content,
        session_key_override=session_key_override,
    )


# ---------------------------------------------------------------------------
# Unit tests — session_key resolution (no bus needed)
# ---------------------------------------------------------------------------


def test_session_key_default() -> None:
    m = _msg(channel="telegram", chat_id="9999")
    assert m.session_key == "telegram:9999"


def test_session_key_canonical_id_wins_over_default() -> None:
    m = _msg(channel="telegram", chat_id="9999", canonical_id="user:alice")
    assert m.session_key == "user:alice"


def test_session_key_override_wins_over_canonical_id() -> None:
    m = _msg(canonical_id="user:alice", session_key_override="special:session")
    assert m.session_key == "special:session"


def test_cross_channel_same_canonical_id() -> None:
    wa = _msg(channel="whatsapp", chat_id="+1234567890", canonical_id="user:alice")
    tg = _msg(channel="telegram", chat_id="12345678", canonical_id="user:alice")
    assert wa.session_key == tg.session_key == "user:alice"


# ---------------------------------------------------------------------------
# Async tests — MessageBus routing
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_publish_routes_to_session_queue() -> None:
    bus = MessageBus()
    await bus.start()

    msg = _msg(channel="telegram", chat_id="42")
    await bus.publish(msg)
    await asyncio.sleep(0.05)  # let fanout run

    q = bus.session_queue("telegram:42")
    assert not q.empty()
    received = q.get_nowait()
    assert received is msg

    await bus.close()


@pytest.mark.asyncio
async def test_cross_channel_messages_share_one_queue() -> None:
    """WhatsApp + Telegram messages with same canonical_id → one session queue."""
    bus = MessageBus()
    await bus.start()

    wa = _msg(channel="whatsapp", chat_id="+1", canonical_id="user:alice", content="hi from wa")
    tg = _msg(channel="telegram", chat_id="99", canonical_id="user:alice", content="hi from tg")

    await bus.publish(wa)
    await bus.publish(tg)
    await asyncio.sleep(0.05)

    q = bus.session_queue("user:alice")
    assert q.qsize() == 2

    r1 = q.get_nowait()
    r2 = q.get_nowait()
    assert {r1.channel, r2.channel} == {"whatsapp", "telegram"}

    await bus.close()


@pytest.mark.asyncio
async def test_different_sessions_get_different_queues() -> None:
    bus = MessageBus()
    await bus.start()

    await bus.publish(_msg(channel="telegram", chat_id="1", content="a"))
    await bus.publish(_msg(channel="telegram", chat_id="2", content="b"))
    await asyncio.sleep(0.05)

    q1 = bus.session_queue("telegram:1")
    q2 = bus.session_queue("telegram:2")
    assert q1 is not q2
    assert q1.qsize() == 1
    assert q2.qsize() == 1

    await bus.close()


@pytest.mark.asyncio
async def test_on_new_session_callback_called_once_per_key() -> None:
    bus = MessageBus()
    seen: list[str] = []
    bus.on_new_session(seen.append)
    await bus.start()

    # Publish two messages for the same session
    for _ in range(2):
        await bus.publish(_msg(channel="slack", chat_id="C123"))
    # Publish one message for a different session
    await bus.publish(_msg(channel="discord", chat_id="D456"))
    await asyncio.sleep(0.05)

    assert seen.count("slack:C123") == 1
    assert seen.count("discord:D456") == 1
    assert len(seen) == 2

    await bus.close()


@pytest.mark.asyncio
async def test_dispatch_puts_on_outbound_queue() -> None:
    bus = MessageBus()
    await bus.start()

    reply = OutboundMessage(channel="telegram", chat_id="42", content="pong")
    await bus.dispatch(reply)

    out = bus.outbound.get_nowait()
    assert out is reply

    await bus.close()


@pytest.mark.asyncio
async def test_active_sessions_snapshot() -> None:
    bus = MessageBus()
    await bus.start()

    await bus.publish(_msg(channel="telegram", chat_id="1"))
    await bus.publish(_msg(channel="telegram", chat_id="2"))
    await asyncio.sleep(0.05)

    sessions = bus.active_sessions()
    assert "telegram:1" in sessions
    assert "telegram:2" in sessions

    await bus.close()


# ---------------------------------------------------------------------------
# Async tests — close / drain
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_close_sends_sentinel_to_session_queues() -> None:
    """After close(), each session queue receives a None sentinel."""
    bus = MessageBus()
    await bus.start()

    await bus.publish(_msg(channel="telegram", chat_id="99"))
    await asyncio.sleep(0.05)

    # Pre-create the queue reference before close
    q = bus.session_queue("telegram:99")
    await bus.close()

    # Drain real message + sentinel
    items = []
    while not q.empty():
        items.append(q.get_nowait())

    assert None in items


@pytest.mark.asyncio
async def test_close_sends_sentinel_to_outbound_queue() -> None:
    bus = MessageBus()
    await bus.start()
    await bus.close()

    sentinel = bus.outbound.get_nowait()
    assert sentinel is None


@pytest.mark.asyncio
async def test_publish_after_close_raises() -> None:
    bus = MessageBus()
    await bus.start()
    await bus.close()

    with pytest.raises(RuntimeError, match="closed"):
        await bus.publish(_msg())


@pytest.mark.asyncio
async def test_dispatch_after_close_raises() -> None:
    bus = MessageBus()
    await bus.start()
    await bus.close()

    with pytest.raises(RuntimeError, match="closed"):
        await bus.dispatch(OutboundMessage(channel="x", chat_id="y", content="z"))


# ---------------------------------------------------------------------------
# Async tests — bounded queue (drop on full)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_full_session_queue_drops_message(caplog: pytest.LogCaptureFixture) -> None:
    """When a session queue is full, the message is dropped with a warning."""
    import logging

    bus = MessageBus(maxsize=2)
    await bus.start()

    # Pre-create the session queue so we can fill it before the fanout runs
    q = bus.session_queue("telegram:1")
    q.put_nowait(_msg())  # fill slot 1
    q.put_nowait(_msg())  # fill slot 2 — queue is now full

    with caplog.at_level(logging.WARNING, logger="openagent.bus.bus"):
        await bus.publish(_msg(channel="telegram", chat_id="1", content="overflow"))
        await asyncio.sleep(0.05)

    assert "full" in caplog.text.lower() or "dropping" in caplog.text.lower()

    await bus.close()
