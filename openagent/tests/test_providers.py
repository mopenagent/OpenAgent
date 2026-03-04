"""Tests for openagent.providers — LLMResponse, chat() with tool_calls."""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from openagent.providers import (
    AnthropicProvider,
    LLMResponse,
    Message,
    OpenAICompatProvider,
    ToolCall,
    get_provider,
)
from openagent.providers.config import ProviderConfig


# ---------------------------------------------------------------------------
# Helpers — build fake httpx responses
# ---------------------------------------------------------------------------


def _oai_response(content: str = "", tool_calls: list | None = None) -> dict:
    """Construct a minimal OpenAI-compatible /chat/completions JSON response."""
    msg: dict = {"content": content}
    if tool_calls:
        msg["tool_calls"] = tool_calls
    return {"choices": [{"message": msg}]}


def _anthropic_response(content: str = "", tool_uses: list | None = None) -> dict:
    """Construct a minimal Anthropic Messages API JSON response."""
    blocks = []
    if content:
        blocks.append({"type": "text", "text": content})
    for tu in (tool_uses or []):
        blocks.append(tu)
    return {"content": blocks}


# ---------------------------------------------------------------------------
# Unit tests — LLMResponse / ToolCall dataclasses
# ---------------------------------------------------------------------------


def test_llm_response_no_tools() -> None:
    r = LLMResponse(content="hello")
    assert r.content == "hello"
    assert r.tool_calls == []
    assert not r.has_tool_calls


def test_llm_response_with_tools() -> None:
    tc = ToolCall(id="c1", name="search", arguments={"q": "pi"})
    r = LLMResponse(content="", tool_calls=[tc])
    assert r.has_tool_calls
    assert r.tool_calls[0].name == "search"
    assert r.tool_calls[0].arguments == {"q": "pi"}


# ---------------------------------------------------------------------------
# Unit tests — OpenAICompatProvider.chat()
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_oai_compat_chat_text_only() -> None:
    cfg = ProviderConfig(base_url="http://localhost:1234/v1", model="test")
    provider = OpenAICompatProvider(cfg)

    fake_resp = MagicMock()
    fake_resp.json.return_value = _oai_response(content="Paris")
    fake_resp.raise_for_status = MagicMock()

    mock_client = AsyncMock()
    mock_client.__aenter__ = AsyncMock(return_value=mock_client)
    mock_client.__aexit__ = AsyncMock(return_value=False)
    mock_client.post = AsyncMock(return_value=fake_resp)

    with patch("httpx.AsyncClient", return_value=mock_client):
        result = await provider.chat([Message("user", "capital of France?")])

    assert result.content == "Paris"
    assert result.tool_calls == []


@pytest.mark.asyncio
async def test_oai_compat_chat_with_tool_calls() -> None:
    cfg = ProviderConfig(base_url="http://localhost:1234/v1", model="test")
    provider = OpenAICompatProvider(cfg)

    raw_calls = [
        {
            "id": "call_abc",
            "function": {
                "name": "web_search",
                "arguments": json.dumps({"query": "weather today"}),
            },
        }
    ]
    fake_resp = MagicMock()
    fake_resp.json.return_value = _oai_response(content="", tool_calls=raw_calls)
    fake_resp.raise_for_status = MagicMock()

    mock_client = AsyncMock()
    mock_client.__aenter__ = AsyncMock(return_value=mock_client)
    mock_client.__aexit__ = AsyncMock(return_value=False)
    mock_client.post = AsyncMock(return_value=fake_resp)

    tools = [{"type": "function", "function": {"name": "web_search", "parameters": {}}}]

    with patch("httpx.AsyncClient", return_value=mock_client):
        result = await provider.chat(
            [Message("user", "what's the weather?")], tools=tools
        )

    assert result.content == ""
    assert len(result.tool_calls) == 1
    tc = result.tool_calls[0]
    assert tc.id == "call_abc"
    assert tc.name == "web_search"
    assert tc.arguments == {"query": "weather today"}


@pytest.mark.asyncio
async def test_oai_compat_chat_malformed_args_fallback() -> None:
    """Malformed JSON in tool_call arguments should not raise — fallback to _raw."""
    cfg = ProviderConfig(base_url="http://localhost:1234/v1", model="test")
    provider = OpenAICompatProvider(cfg)

    raw_calls = [
        {"id": "c1", "function": {"name": "tool_x", "arguments": "NOT JSON"}}
    ]
    fake_resp = MagicMock()
    fake_resp.json.return_value = _oai_response(tool_calls=raw_calls)
    fake_resp.raise_for_status = MagicMock()

    mock_client = AsyncMock()
    mock_client.__aenter__ = AsyncMock(return_value=mock_client)
    mock_client.__aexit__ = AsyncMock(return_value=False)
    mock_client.post = AsyncMock(return_value=fake_resp)

    with patch("httpx.AsyncClient", return_value=mock_client):
        result = await provider.chat([Message("user", "go")])

    assert result.tool_calls[0].arguments == {"_raw": "NOT JSON"}


# ---------------------------------------------------------------------------
# Unit tests — AnthropicProvider.chat()
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_anthropic_chat_text_only() -> None:
    cfg = ProviderConfig(kind="anthropic", api_key="sk-test", model="claude-haiku-4-5-20251001")
    provider = AnthropicProvider(cfg)

    fake_resp = MagicMock()
    fake_resp.json.return_value = _anthropic_response(content="London")
    fake_resp.raise_for_status = MagicMock()

    mock_client = AsyncMock()
    mock_client.__aenter__ = AsyncMock(return_value=mock_client)
    mock_client.__aexit__ = AsyncMock(return_value=False)
    mock_client.post = AsyncMock(return_value=fake_resp)

    with patch("httpx.AsyncClient", return_value=mock_client):
        result = await provider.chat([Message("user", "capital of UK?")])

    assert result.content == "London"
    assert not result.has_tool_calls


@pytest.mark.asyncio
async def test_anthropic_chat_tool_use() -> None:
    cfg = ProviderConfig(kind="anthropic", api_key="sk-test", model="claude-haiku-4-5-20251001")
    provider = AnthropicProvider(cfg)

    tool_use_block = {
        "type": "tool_use",
        "id": "toolu_01",
        "name": "calculator",
        "input": {"expression": "2+2"},
    }
    fake_resp = MagicMock()
    fake_resp.json.return_value = _anthropic_response(tool_uses=[tool_use_block])
    fake_resp.raise_for_status = MagicMock()

    mock_client = AsyncMock()
    mock_client.__aenter__ = AsyncMock(return_value=mock_client)
    mock_client.__aexit__ = AsyncMock(return_value=False)
    mock_client.post = AsyncMock(return_value=fake_resp)

    tools = [{"name": "calculator", "description": "eval math", "params": {}}]

    with patch("httpx.AsyncClient", return_value=mock_client):
        result = await provider.chat([Message("user", "what is 2+2?")], tools=tools)

    assert result.has_tool_calls
    tc = result.tool_calls[0]
    assert tc.id == "toolu_01"
    assert tc.name == "calculator"
    assert tc.arguments == {"expression": "2+2"}


@pytest.mark.asyncio
async def test_anthropic_tool_schema_conversion() -> None:
    """Verify Anthropic format conversion (input_schema, not parameters)."""
    cfg = ProviderConfig(kind="anthropic", api_key="sk-test")
    provider = AnthropicProvider(cfg)

    oai_tools = [
        {
            "type": "function",
            "function": {
                "name": "search",
                "description": "search the web",
                "parameters": {"type": "object", "properties": {"q": {"type": "string"}}},
            },
        }
    ]
    converted = provider._tools_to_anthropic(oai_tools)
    assert len(converted) == 1
    assert converted[0]["name"] == "search"
    assert "input_schema" in converted[0]
    assert converted[0]["input_schema"]["properties"]["q"]["type"] == "string"


# ---------------------------------------------------------------------------
# Unit tests — get_provider factory
# ---------------------------------------------------------------------------


def test_get_provider_openai_compat() -> None:
    cfg = ProviderConfig(kind="openai_compat", base_url="http://localhost/v1")
    p = get_provider(cfg)
    assert isinstance(p, OpenAICompatProvider)


def test_get_provider_anthropic() -> None:
    cfg = ProviderConfig(kind="anthropic", api_key="sk-x")
    p = get_provider(cfg)
    assert isinstance(p, AnthropicProvider)


def test_get_provider_message_with_tool_role() -> None:
    """Message with role=tool should carry tool_call_id and tool_name."""
    m = Message(role="tool", content="42", tool_call_id="c1", tool_name="calculator")
    assert m.role == "tool"
    assert m.tool_call_id == "c1"
    assert m.tool_name == "calculator"
