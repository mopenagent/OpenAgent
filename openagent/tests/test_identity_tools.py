"""Tests for identity_tools + ToolRegistry native tool support."""

from __future__ import annotations

import json
from datetime import datetime, timedelta
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest
import pytest_asyncio

from openagent.agent.identity_tools import make_identity_tools
from openagent.agent.tools import ToolRegistry
from openagent.session import SessionManager, SqliteSessionBackend


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest_asyncio.fixture
async def sessions(tmp_path: Path) -> SessionManager:
    b = SqliteSessionBackend(tmp_path / "id.db")
    m = SessionManager(backend=b, summarise_after=0)
    await m.start()
    yield m
    await m.stop()


@pytest_asyncio.fixture
def registry(sessions: SessionManager) -> ToolRegistry:
    svc_mgr = MagicMock()
    svc_mgr.list_services.return_value = []
    reg = ToolRegistry(svc_mgr)
    for name, desc, params, fn in make_identity_tools(sessions):
        reg.register_native(name, desc, params, fn)
    return reg


# ---------------------------------------------------------------------------
# ToolRegistry — native tool registration
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_register_native_appears_in_schemas(registry: ToolRegistry) -> None:
    names = {s["function"]["name"] for s in registry.schemas()}
    assert "identity.generate_link_pin" in names
    assert "identity.redeem_link_pin" in names


@pytest.mark.asyncio
async def test_has_tools_true_with_only_native(registry: ToolRegistry) -> None:
    assert registry.has_tools()


@pytest.mark.asyncio
async def test_native_tools_survive_rebuild(registry: ToolRegistry) -> None:
    await registry.rebuild()
    names = {s["function"]["name"] for s in registry.schemas()}
    assert "identity.generate_link_pin" in names


# ---------------------------------------------------------------------------
# identity.generate_link_pin
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_generate_pin_returns_six_digits(registry: ToolRegistry, sessions: SessionManager) -> None:
    session_key = await sessions.resolve_user_key("telegram", "1")
    result = await registry.call("identity.generate_link_pin", {}, session_key=session_key)
    data = json.loads(result)
    assert "pin" in data
    assert len(data["pin"]) == 6
    assert data["pin"].isdigit()


@pytest.mark.asyncio
async def test_generate_pin_stores_in_backend(registry: ToolRegistry, sessions: SessionManager) -> None:
    session_key = await sessions.resolve_user_key("telegram", "2")
    result = await registry.call("identity.generate_link_pin", {}, session_key=session_key)
    pin = json.loads(result)["pin"]

    # Redeem it from another session to confirm it was stored
    other_key = await sessions.resolve_user_key("discord", "D2")
    winner = await sessions.redeem_link_pin(other_key, pin)
    assert winner == session_key


# ---------------------------------------------------------------------------
# identity.redeem_link_pin
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_redeem_pin_links_sessions(registry: ToolRegistry, sessions: SessionManager) -> None:
    key_a = await sessions.resolve_user_key("telegram", "3")
    key_b = await sessions.resolve_user_key("discord", "D3")

    # Generate pin from key_a session
    gen_result = await registry.call(
        "identity.generate_link_pin", {}, session_key=key_a
    )
    pin = json.loads(gen_result)["pin"]

    # Redeem from key_b session
    redeem_result = await registry.call(
        "identity.redeem_link_pin", {"pin": pin}, session_key=key_b
    )
    data = json.loads(redeem_result)
    assert data.get("ok") is True
    assert data["session_key"] == key_a


@pytest.mark.asyncio
async def test_redeem_wrong_pin_returns_error(registry: ToolRegistry, sessions: SessionManager) -> None:
    key_b = await sessions.resolve_user_key("slack", "U9")
    result = await registry.call(
        "identity.redeem_link_pin", {"pin": "000000"}, session_key=key_b
    )
    data = json.loads(result)
    assert "error" in data


@pytest.mark.asyncio
async def test_redeem_missing_pin_arg_returns_error(registry: ToolRegistry, sessions: SessionManager) -> None:
    key_b = await sessions.resolve_user_key("slack", "U10")
    result = await registry.call(
        "identity.redeem_link_pin", {}, session_key=key_b
    )
    data = json.loads(result)
    assert "error" in data
