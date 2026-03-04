from __future__ import annotations

import pytest

from openagent.services.protocol import (
    EventFrame,
    ProtocolPong,
    ToolCallRequest,
    ToolListResponse,
    parse_frame,
)


def test_parse_tool_call_request() -> None:
    frame = parse_frame(
        '{"id":"req-1","type":"tool.call","tool":"echo","params":{"text":"hi"}}'
    )
    assert isinstance(frame, ToolCallRequest)
    assert frame.id == "req-1"
    assert frame.tool == "echo"
    assert frame.params == {"text": "hi"}


def test_parse_tools_list_response() -> None:
    frame = parse_frame(
        {
            "id": "req-2",
            "type": "tools.list.ok",
            "tools": [
                {
                    "name": "sum",
                    "description": "Adds numbers",
                    "params": {"type": "object", "properties": {}},
                }
            ],
        }
    )
    assert isinstance(frame, ToolListResponse)
    assert frame.tools[0].name == "sum"


def test_parse_event_frame() -> None:
    frame = parse_frame(
        '{"type":"event","event":"message.received","data":{"chat_id":"abc"}}'
    )
    assert isinstance(frame, EventFrame)
    assert frame.event == "message.received"
    assert frame.data["chat_id"] == "abc"


def test_parse_pong_frame() -> None:
    frame = parse_frame('{"id":"health-1","type":"pong","status":"ready"}')
    assert isinstance(frame, ProtocolPong)
    assert frame.status == "ready"


def test_parse_invalid_frame_rejected() -> None:
    with pytest.raises(ValueError):
        parse_frame('{"id":"x","type":"unknown"}')
