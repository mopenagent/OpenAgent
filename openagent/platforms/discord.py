"""Discord platform adapter backed by services/discord MCP-lite service."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from openagent.services import protocol as proto

from .mcplite import McpLiteClient


class DiscordServicePlatform(McpLiteClient):
    """Python-side connector for the Discord Go service."""

    def __init__(self, *, socket_path: str | Path = "data/sockets/discord.sock"):
        super().__init__(socket_path=socket_path)
        self._status: dict[str, Any] = {
            "running": False,
            "connected": False,
            "authorized": False,
            "backend": "discordgo",
            "service_socket": str(socket_path),
        }
        self._messages: list[dict[str, Any]] = []

    async def start(self) -> None:
        await super().start()
        self._status["running"] = True

    async def stop(self) -> None:
        await super().stop()
        self._status["running"] = False
        self._status["connected"] = False
        self._status["authorized"] = False

    async def get_status(self) -> dict[str, Any]:
        frame = await self.request({"type": "tool.call", "tool": "discord.status", "params": {}})
        payload = _decode_tool_result(frame)
        self._status.update(payload)
        return dict(self._status)

    async def get_link_state(self) -> dict[str, Any]:
        frame = await self.request({"type": "tool.call", "tool": "discord.link_state", "params": {}})
        payload = _decode_tool_result(frame)
        self._status.update(payload)
        return payload

    async def send_message(self, platform_id: str, text: str) -> dict[str, Any]:
        frame = await self.request(
            {
                "type": "tool.call",
                "tool": "discord.send_message",
                "params": {"platform_id": platform_id, "text": text},
            }
        )
        return _decode_tool_result(frame)

    def pop_messages(self) -> list[dict[str, Any]]:
        batch = list(self._messages)
        self._messages.clear()
        return batch

    def on_event(self, frame: proto.EventFrame) -> None:
        if frame.event == "discord.connection.status":
            self._status.update(frame.data)
            return
        if frame.event == "discord.message.received":
            self._messages.append(dict(frame.data))


def _decode_tool_result(frame: proto.FrameModel) -> dict[str, Any]:
    if not isinstance(frame, proto.ToolResultResponse):
        raise RuntimeError(f"unexpected response type: {type(frame).__name__}")
    if frame.error:
        raise RuntimeError(frame.error)
    if frame.result is None:
        return {}
    try:
        payload = json.loads(frame.result)
    except json.JSONDecodeError:
        return {"result": frame.result}
    if isinstance(payload, dict):
        return payload
    return {"result": payload}
