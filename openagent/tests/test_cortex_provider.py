from __future__ import annotations

import json
from unittest.mock import AsyncMock

import pytest

from openagent.providers.base import Message
from openagent.providers.cortex import CortexProvider, _render_transcript
from openagent.services.protocol import ToolResultResponse


def test_render_transcript_skips_system_and_labels_tools() -> None:
    transcript = _render_transcript([
        Message("system", "ignored"),
        Message("user", "hello"),
        Message("assistant", "hi"),
        Message("tool", "42", tool_name="calculator"),
    ])
    assert "SYSTEM" not in transcript
    assert "USER: hello" in transcript
    assert "ASSISTANT: hi" in transcript
    assert "TOOL:calculator: 42" in transcript


@pytest.mark.asyncio
async def test_cortex_provider_chat_routes_to_cortex_step() -> None:
    client = AsyncMock()
    client.request = AsyncMock(
        return_value=ToolResultResponse(
            id="1",
            type="tool.result",
            result=json.dumps({"response_text": "world"}),
            error=None,
        )
    )
    provider = CortexProvider(get_client=lambda: client, default_agent_name="AgentM")

    result = await provider.chat(
        [Message("system", "sys"), Message("user", "hello")],
        session_key="web:123",
    )

    assert result.content == "world"
    payload = client.request.await_args.args[0]
    assert payload["tool"] == "cortex.step"
    assert payload["params"]["session_id"] == "web:123"
    assert payload["params"]["agent_name"] == "AgentM"
    assert "USER: hello" in payload["params"]["user_input"]
