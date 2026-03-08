"""Tests for openagent.agent — AgentLoop + ToolRegistry."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
import pytest_asyncio

from openagent.agent.loop import AgentLoop, MAX_ITERATIONS, MAX_TOOL_OUTPUT
from openagent.agent.tools import ToolRegistry, _to_openai_schema
from openagent.bus.bus import MessageBus
from openagent.bus.events import InboundMessage, SenderInfo
from openagent.providers.base import LLMResponse, Message, StreamEvent, ToolCall
from openagent.services import protocol as proto
from openagent.session import SessionManager, SqliteSessionBackend


# ---------------------------------------------------------------------------
# Helpers — fake provider
# ---------------------------------------------------------------------------


class _FakeProvider:
    """Records calls and returns pre-configured responses via stream_with_tools.

    Each call to stream_with_tools consumes one LLMResponse from the queue and
    yields the appropriate StreamEvents so the unified loop path is exercised.
    """

    def __init__(self, responses: list[LLMResponse]) -> None:
        self._responses = iter(responses)
        self.calls: list[tuple[list[Message], Any]] = []

    async def stream_with_tools(
        self,
        messages: list[Message],
        *,
        tools: list | None = None,
        **kwargs,
    ):
        self.calls.append((messages, tools))
        try:
            response = next(self._responses)
        except StopIteration:
            response = LLMResponse(content="(no more responses)")
        if response.content:
            yield StreamEvent(content=response.content)
        if response.tool_calls:
            yield StreamEvent(tool_calls=response.tool_calls, finish_reason="tool_calls")

    async def chat(
        self,
        messages: list[Message],
        tools: list | None = None,
        **kwargs,
    ) -> LLMResponse:
        self.calls.append((messages, tools))
        try:
            return next(self._responses)
        except StopIteration:
            return LLMResponse(content="(no more responses)")

    async def stream(self, messages, **kwargs):
        try:
            response = next(self._responses)
            yield response.content or ""
        except StopIteration:
            yield ""

    async def complete(self, messages, **kwargs) -> str:
        return ""


def _inbound(content: str = "hello", platform: str = "telegram", channel_id: str = "1") -> InboundMessage:
    return InboundMessage(
        platform=platform,
        sender=SenderInfo(platform=platform, user_id="u1"),
        channel_id=channel_id,
        content=content,
    )


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest_asyncio.fixture
async def session_mgr(tmp_path: Path) -> SessionManager:
    b = SqliteSessionBackend(tmp_path / "s.db")
    m = SessionManager(backend=b, summarise_after=0)
    await m.start()
    yield m
    await m.stop()


@pytest_asyncio.fixture
async def bus() -> MessageBus:
    b = MessageBus()
    await b.start()
    yield b
    await b.close()


def _empty_registry() -> ToolRegistry:
    mgr = MagicMock()
    mgr.list_services.return_value = []
    return ToolRegistry(mgr)


# ---------------------------------------------------------------------------
# ToolRegistry tests
# ---------------------------------------------------------------------------


def test_to_openai_schema() -> None:
    td = proto.ToolDefinition(
        name="search",
        description="search the web",
        params={"type": "object", "properties": {"q": {"type": "string"}}},
    )
    schema = _to_openai_schema(td)
    assert schema["type"] == "function"
    assert schema["function"]["name"] == "search"
    assert "q" in schema["function"]["parameters"]["properties"]


@pytest.mark.asyncio
async def test_registry_rebuild_empty_when_no_services() -> None:
    mgr = MagicMock()
    mgr.list_services.return_value = []
    registry = ToolRegistry(mgr)
    await registry.rebuild()
    assert registry.schemas() == []
    assert not registry.has_tools()


@pytest.mark.asyncio
async def test_registry_rebuild_with_running_service() -> None:
    tool = proto.ToolDefinition(name="ping", description="ping", params={})
    tool_response = proto.ToolListResponse(id="r1", type="tools.list.ok", tools=[tool])

    client = AsyncMock()
    client.request = AsyncMock(return_value=tool_response)

    svc = MagicMock()
    svc.name = "discord"

    mgr = MagicMock()
    mgr.list_services.return_value = [svc]
    mgr.get_client.return_value = client

    registry = ToolRegistry(mgr)
    await registry.rebuild()

    assert registry.has_tools()
    schemas = registry.schemas()
    assert len(schemas) == 1
    assert schemas[0]["function"]["name"] == "ping"


@pytest.mark.asyncio
async def test_registry_call_returns_result() -> None:
    result_frame = proto.ToolResultResponse(id="r1", type="tool.result", result="pong")
    client = AsyncMock()
    client.request = AsyncMock(return_value=result_frame)

    svc = MagicMock()
    svc.name = "discord"

    mgr = MagicMock()
    mgr.list_services.return_value = [svc]
    mgr.get_client.return_value = client

    registry = ToolRegistry(mgr)
    registry._tool_to_service["ping"] = "discord"

    result = await registry.call("ping", {})
    assert result == "pong"


@pytest.mark.asyncio
async def test_registry_call_unknown_tool() -> None:
    mgr = MagicMock()
    mgr.list_services.return_value = []
    registry = ToolRegistry(mgr)
    result = await registry.call("unknown_tool", {})
    assert "unknown tool" in result


@pytest.mark.asyncio
async def test_registry_call_service_not_running() -> None:
    mgr = MagicMock()
    mgr.get_client.return_value = None
    registry = ToolRegistry(mgr)
    registry._tool_to_service["ping"] = "dead_service"
    result = await registry.call("ping", {})
    assert "not running" in result


# ---------------------------------------------------------------------------
# AgentLoop tests
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_agent_loop_simple_reply(
    bus: MessageBus,
    session_mgr: SessionManager,
) -> None:
    """A single text reply with no tool calls reaches the outbound queue."""
    provider = _FakeProvider([LLMResponse(content="world")])
    registry = _empty_registry()
    loop = AgentLoop(bus, provider, session_mgr, registry, middlewares=[])
    await loop.start()

    msg = _inbound("hello")
    await bus.publish(msg)
    await asyncio.sleep(0.1)

    out = bus.outbound.get_nowait()
    assert out is not None
    assert out.content == "world"
    assert out.platform == "telegram"

    await loop.stop()


@pytest.mark.asyncio
async def test_agent_loop_saves_turns_to_session(
    bus: MessageBus,
    session_mgr: SessionManager,
) -> None:
    provider = _FakeProvider([LLMResponse(content="answer")])
    registry = _empty_registry()
    loop = AgentLoop(bus, provider, session_mgr, registry, middlewares=[])
    await loop.start()

    msg = _inbound("question", channel_id="99")
    await bus.publish(msg)
    await asyncio.sleep(0.1)

    history = await session_mgr.get_history("telegram:99")
    roles = [t.role for t in history]
    assert "user" in roles
    assert "assistant" in roles

    await loop.stop()


@pytest.mark.asyncio
async def test_agent_loop_tool_call_dispatched(
    bus: MessageBus,
    session_mgr: SessionManager,
) -> None:
    """LLM emits a tool_call → tool executes → LLM gets result → final answer."""
    tc = ToolCall(id="c1", name="add", arguments={"a": 1, "b": 2})
    provider = _FakeProvider([
        LLMResponse(content="", tool_calls=[tc]),  # first: tool call
        LLMResponse(content="The answer is 3"),    # second: final
    ])

    # Registry mock that returns "3" for "add"
    mgr_mock = MagicMock()
    mgr_mock.list_services.return_value = []
    registry = ToolRegistry(mgr_mock)
    registry._tool_to_service["add"] = "calc_service"
    registry._schemas = [{"type": "function", "function": {"name": "add", "description": "add two numbers", "parameters": {"type": "object", "properties": {}}}}]

    client_mock = AsyncMock()
    result_frame = proto.ToolResultResponse(id="r1", type="tool.result", result="3")
    client_mock.request = AsyncMock(return_value=result_frame)
    mgr_mock.get_client.return_value = client_mock

    loop = AgentLoop(bus, provider, session_mgr, registry, middlewares=[])
    await loop.start()

    await bus.publish(_inbound("what is 1+2?"))
    await asyncio.sleep(0.2)

    out = bus.outbound.get_nowait()
    assert "3" in out.content

    # Tool result should be in session history
    history = await session_mgr.get_history("telegram:1")
    roles = [t.role for t in history]
    assert "tool" in roles

    await loop.stop()


@pytest.mark.asyncio
async def test_agent_loop_tool_output_truncated(
    bus: MessageBus,
    session_mgr: SessionManager,
) -> None:
    """Tool output longer than MAX_TOOL_OUTPUT is truncated."""
    long_result = "x" * (MAX_TOOL_OUTPUT + 100)
    tc = ToolCall(id="c1", name="big_tool", arguments={})
    provider = _FakeProvider([
        LLMResponse(content="", tool_calls=[tc]),
        LLMResponse(content="done"),
    ])

    mgr_mock = MagicMock()
    mgr_mock.list_services.return_value = []
    registry = ToolRegistry(mgr_mock)
    registry._tool_to_service["big_tool"] = "svc"
    registry._schemas = [{"type": "function", "function": {"name": "big_tool", "description": "big tool", "parameters": {"type": "object", "properties": {}}}}]

    result_frame = proto.ToolResultResponse(id="r1", type="tool.result", result=long_result)
    client_mock = AsyncMock()
    client_mock.request = AsyncMock(return_value=result_frame)
    mgr_mock.get_client.return_value = client_mock

    loop = AgentLoop(bus, provider, session_mgr, registry, middlewares=[])
    await loop.start()

    await bus.publish(_inbound("fetch lots of data"))
    await asyncio.sleep(0.2)

    history = await session_mgr.get_history("telegram:1")
    tool_turns = [t for t in history if t.role == "tool"]
    assert tool_turns
    assert len(tool_turns[0].content) <= MAX_TOOL_OUTPUT + 20  # +20 for "…[truncated]"
    assert "truncated" in tool_turns[0].content

    await loop.stop()


@pytest.mark.asyncio
async def test_agent_loop_cross_platform_same_session(
    bus: MessageBus,
    session_mgr: SessionManager,
) -> None:
    """Messages from WhatsApp and Telegram with same user_key share history."""
    provider = _FakeProvider([
        LLMResponse(content="reply1"),
        LLMResponse(content="reply2"),
    ])
    registry = _empty_registry()
    loop = AgentLoop(bus, provider, session_mgr, registry, middlewares=[])
    await loop.start()

    wa = InboundMessage(
        platform="whatsapp", channel_id="+1",
        sender=SenderInfo("whatsapp", "+1", user_key="user:alice"),
        content="from whatsapp",
    )
    tg = InboundMessage(
        platform="telegram", channel_id="99",
        sender=SenderInfo("telegram", "99", user_key="user:alice"),
        content="from telegram",
    )

    await bus.publish(wa)
    await asyncio.sleep(0.1)
    await bus.publish(tg)
    await asyncio.sleep(0.1)

    history = await session_mgr.get_history("user:alice")
    contents = [t.content for t in history if t.role == "user"]
    assert "from whatsapp" in contents
    assert "from telegram" in contents

    await loop.stop()


@pytest.mark.asyncio
async def test_agent_loop_stop_cancels_tasks(
    bus: MessageBus,
    session_mgr: SessionManager,
) -> None:
    """stop() cancels all per-session worker tasks cleanly."""
    # Provider blocks forever inside stream_with_tools to keep the task alive
    async def _blocking_stream(messages, *, tools=None, **kw):
        await asyncio.sleep(100)
        yield StreamEvent(content="never")

    provider = MagicMock()
    provider.stream_with_tools = _blocking_stream

    registry = _empty_registry()
    loop = AgentLoop(bus, provider, session_mgr, registry, middlewares=[])
    await loop.start()

    await bus.publish(_inbound("trigger session"))
    await asyncio.sleep(0.05)  # let session worker start

    assert loop._tasks  # at least one task running
    await loop.stop()
    assert not loop._tasks
