"""Tests for openagent.session — backend, manager, summarisation."""

from __future__ import annotations

import asyncio
from pathlib import Path

import pytest
import pytest_asyncio

from openagent.session import SessionManager, SqliteSessionBackend, Turn
from openagent.providers.base import Message


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest_asyncio.fixture
async def backend(tmp_path: Path) -> SqliteSessionBackend:
    b = SqliteSessionBackend(tmp_path / "test.db")
    await b.start()
    yield b
    await b.stop()


@pytest_asyncio.fixture
async def mgr(tmp_path: Path) -> SessionManager:
    b = SqliteSessionBackend(tmp_path / "sessions.db")
    m = SessionManager(backend=b, summarise_after=0)  # no auto-summarise by default
    await m.start()
    yield m
    await m.stop()


# ---------------------------------------------------------------------------
# SqliteSessionBackend — unit tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_append_and_get_history(backend: SqliteSessionBackend) -> None:
    await backend.append("user:alice", "user", "hello")
    await backend.append("user:alice", "assistant", "hi there")
    turns = await backend.get_history("user:alice")
    assert len(turns) == 2
    assert turns[0].role == "user"
    assert turns[0].content == "hello"
    assert turns[1].role == "assistant"


@pytest.mark.asyncio
async def test_history_limit(backend: SqliteSessionBackend) -> None:
    for i in range(10):
        await backend.append("key:1", "user", f"msg {i}")
    turns = await backend.get_history("key:1", limit=3)
    assert len(turns) == 3
    # Should be the last 3, oldest first
    assert turns[-1].content == "msg 9"


@pytest.mark.asyncio
async def test_history_empty_session(backend: SqliteSessionBackend) -> None:
    turns = await backend.get_history("nonexistent:key")
    assert turns == []


@pytest.mark.asyncio
async def test_set_summary_replaces_turns(backend: SqliteSessionBackend) -> None:
    await backend.append("s1", "user", "a")
    await backend.append("s1", "assistant", "b")
    await backend.set_summary("s1", "User said a, assistant said b.")
    turns = await backend.get_history("s1")
    assert len(turns) == 1
    assert turns[0].role == "system"
    assert "Summary" in turns[0].content


@pytest.mark.asyncio
async def test_clear(backend: SqliteSessionBackend) -> None:
    await backend.append("s2", "user", "hello")
    await backend.clear("s2")
    assert await backend.get_history("s2") == []


@pytest.mark.asyncio
async def test_list_sessions(backend: SqliteSessionBackend) -> None:
    await backend.append("alice", "user", "hi")
    await backend.append("bob", "user", "hey")
    sessions = await backend.list_sessions()
    assert "alice" in sessions
    assert "bob" in sessions


@pytest.mark.asyncio
async def test_tool_turn_round_trip(backend: SqliteSessionBackend) -> None:
    await backend.append(
        "s3", "tool", "42",
        tool_call_id="call_1",
        tool_name="calculator",
    )
    turns = await backend.get_history("s3")
    assert turns[0].tool_call_id == "call_1"
    assert turns[0].tool_name == "calculator"


@pytest.mark.asyncio
async def test_isolation_between_sessions(backend: SqliteSessionBackend) -> None:
    await backend.append("sess:a", "user", "from a")
    await backend.append("sess:b", "user", "from b")
    a_turns = await backend.get_history("sess:a")
    b_turns = await backend.get_history("sess:b")
    assert len(a_turns) == 1
    assert len(b_turns) == 1
    assert a_turns[0].content == "from a"
    assert b_turns[0].content == "from b"


# ---------------------------------------------------------------------------
# SessionManager — unit tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_manager_append_and_history(mgr: SessionManager) -> None:
    await mgr.append("u:1", "user", "hello manager")
    history = await mgr.get_history("u:1")
    assert len(history) == 1
    assert history[0].content == "hello manager"


@pytest.mark.asyncio
async def test_manager_to_messages(mgr: SessionManager) -> None:
    await mgr.append("u:2", "user", "ping")
    await mgr.append("u:2", "assistant", "pong")
    history = await mgr.get_history("u:2")
    messages = mgr.to_messages(history)
    assert isinstance(messages[0], Message)
    assert messages[0].role == "user"
    assert messages[1].role == "assistant"


@pytest.mark.asyncio
async def test_manager_auto_summarise_fires(tmp_path: Path) -> None:
    """When summarise_after turns are reached, summarise_fn is called."""
    calls: list[list[Turn]] = []

    async def fake_summarise(turns: list[Turn]) -> str:
        calls.append(turns)
        return "summary text"

    b = SqliteSessionBackend(tmp_path / "sum.db")
    mgr = SessionManager(backend=b, summarise_after=3, summarise_fn=fake_summarise)
    await mgr.start()

    # Append 3 turns — should trigger on the 3rd
    await mgr.append("s:1", "user", "a")
    await mgr.append("s:1", "assistant", "b")
    await mgr.append("s:1", "user", "c")  # fires here

    assert len(calls) == 1
    history = await mgr.get_history("s:1")
    assert len(history) == 1
    assert "Summary" in history[0].content

    await mgr.stop()


@pytest.mark.asyncio
async def test_manager_summarise_without_fn_logs_warning(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    import logging

    b = SqliteSessionBackend(tmp_path / "warn.db")
    mgr = SessionManager(backend=b, summarise_after=2, summarise_fn=None)
    await mgr.start()

    with caplog.at_level(logging.WARNING, logger="openagent.session.manager"):
        await mgr.append("s:w", "user", "a")
        await mgr.append("s:w", "user", "b")

    assert "summarise_fn" in caplog.text or "summarise" in caplog.text.lower()
    await mgr.stop()


@pytest.mark.asyncio
async def test_manager_list_and_clear(mgr: SessionManager) -> None:
    await mgr.append("x:1", "user", "hello")
    await mgr.append("x:2", "user", "world")
    sessions = await mgr.list_sessions()
    assert "x:1" in sessions
    await mgr.clear("x:1")
    assert await mgr.get_history("x:1") == []


# ---------------------------------------------------------------------------
# Cross-channel identity — SqliteSessionBackend
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_resolve_user_key_new_user(backend: SqliteSessionBackend) -> None:
    key = await backend.resolve_user_key("telegram", "12345")
    assert key.startswith("user:")


@pytest.mark.asyncio
async def test_resolve_user_key_stable(backend: SqliteSessionBackend) -> None:
    key1 = await backend.resolve_user_key("discord", "user-abc")
    key2 = await backend.resolve_user_key("discord", "user-abc")
    assert key1 == key2


@pytest.mark.asyncio
async def test_resolve_user_key_different_channels(backend: SqliteSessionBackend) -> None:
    k1 = await backend.resolve_user_key("telegram", "999")
    k2 = await backend.resolve_user_key("slack", "999")
    # Same numeric id on different platforms → different identities
    assert k1 != k2


@pytest.mark.asyncio
async def test_link_user_keys_merges_turns(backend: SqliteSessionBackend) -> None:
    key_a = await backend.resolve_user_key("telegram", "100")
    key_b = await backend.resolve_user_key("discord", "200")
    await backend.append(key_a, "user", "hello from telegram")
    await backend.append(key_b, "user", "hello from discord")

    winner = await backend.link_user_keys(key_a, key_b)
    assert winner == key_a

    turns = await backend.get_history(key_a)
    contents = {t.content for t in turns}
    assert "hello from telegram" in contents
    assert "hello from discord" in contents
    assert await backend.get_history(key_b) == []


@pytest.mark.asyncio
async def test_link_user_keys_redirects_identity(backend: SqliteSessionBackend) -> None:
    key_a = await backend.resolve_user_key("whatsapp", "+111")
    key_b = await backend.resolve_user_key("slack", "U999")
    await backend.link_user_keys(key_a, key_b)

    resolved = await backend.resolve_user_key("slack", "U999")
    assert resolved == key_a


@pytest.mark.asyncio
async def test_store_and_redeem_pin(backend: SqliteSessionBackend) -> None:
    from datetime import datetime, timedelta

    key_a = await backend.resolve_user_key("telegram", "1")
    key_b = await backend.resolve_user_key("discord", "2")
    expires_at = (datetime.now() + timedelta(minutes=10)).isoformat()
    await backend.store_link_pin(key_a, "123456", expires_at)

    winner = await backend.redeem_link_pin(key_b, "123456")
    assert winner == key_a
    # key_b identity now resolves to key_a
    assert await backend.resolve_user_key("discord", "2") == key_a


@pytest.mark.asyncio
async def test_redeem_expired_pin(backend: SqliteSessionBackend) -> None:
    from datetime import datetime, timedelta

    key_a = await backend.resolve_user_key("telegram", "10")
    key_b = await backend.resolve_user_key("discord", "20")
    expired = (datetime.now() - timedelta(seconds=1)).isoformat()
    await backend.store_link_pin(key_a, "999999", expired)

    assert await backend.redeem_link_pin(key_b, "999999") is None


@pytest.mark.asyncio
async def test_redeem_invalid_pin(backend: SqliteSessionBackend) -> None:
    key_b = await backend.resolve_user_key("discord", "30")
    assert await backend.redeem_link_pin(key_b, "000000") is None


@pytest.mark.asyncio
async def test_pin_is_one_time_use(backend: SqliteSessionBackend) -> None:
    from datetime import datetime, timedelta

    key_a = await backend.resolve_user_key("telegram", "40")
    key_b = await backend.resolve_user_key("slack", "U40")
    key_c = await backend.resolve_user_key("whatsapp", "+40")
    expires_at = (datetime.now() + timedelta(minutes=5)).isoformat()
    await backend.store_link_pin(key_a, "777777", expires_at)

    assert await backend.redeem_link_pin(key_b, "777777") is not None
    # Pin consumed — second redeem fails
    assert await backend.redeem_link_pin(key_c, "777777") is None


@pytest.mark.asyncio
async def test_cannot_link_session_to_itself(backend: SqliteSessionBackend) -> None:
    from datetime import datetime, timedelta

    key = await backend.resolve_user_key("telegram", "50")
    expires_at = (datetime.now() + timedelta(minutes=5)).isoformat()
    await backend.store_link_pin(key, "111111", expires_at)
    assert await backend.redeem_link_pin(key, "111111") is None


@pytest.mark.asyncio
async def test_session_manager_proxies_identity(tmp_path: Path) -> None:
    b = SqliteSessionBackend(tmp_path / "id.db")
    m = SessionManager(backend=b, summarise_after=0)
    await m.start()
    key = await m.resolve_user_key("telegram", "77")
    assert key.startswith("user:")
    assert await m.resolve_user_key("telegram", "77") == key
    await m.stop()
