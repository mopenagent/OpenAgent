from __future__ import annotations

import asyncio
import json
from pathlib import Path
import uuid

import pytest

from openagent.channels.mcplite import McpLiteClient


class _RecordingClient(McpLiteClient):
    def __init__(self, *, socket_path: str | Path):
        super().__init__(socket_path=socket_path)
        self.events = []

    def on_event(self, frame):
        self.events.append(frame)


def test_mcplite_client_request_and_event(tmp_path: Path):
    socket_path = Path("/tmp") / f"oa-mcplite-{uuid.uuid4().hex[:8]}.sock"

    async def handler(reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
        while True:
            line = await reader.readline()
            if not line:
                break
            req = json.loads(line.decode("utf-8"))
            if req["type"] == "tools.list":
                writer.write(
                    (
                        json.dumps(
                            {
                                "id": req["id"],
                                "type": "tools.list.ok",
                                "tools": [],
                            }
                        )
                        + "\n"
                    ).encode("utf-8")
                )
                writer.write(
                    (
                        json.dumps(
                            {
                                "type": "event",
                                "event": "test.event",
                                "data": {"x": 1},
                            }
                        )
                        + "\n"
                    ).encode("utf-8")
                )
            elif req["type"] == "tool.call":
                writer.write(
                    (
                        json.dumps(
                            {
                                "id": req["id"],
                                "type": "tool.result",
                                "result": "ok",
                                "error": None,
                            }
                        )
                        + "\n"
                    ).encode("utf-8")
                )
            await writer.drain()
        writer.close()
        await writer.wait_closed()

    async def scenario():
        try:
            server = await asyncio.start_unix_server(handler, path=str(socket_path))
        except PermissionError as exc:
            pytest.skip(f"unix socket bind not permitted in sandbox: {exc}")
        try:
            client = _RecordingClient(socket_path=socket_path)
            await client.start()
            assert client.running is True
            frame = await client.request(
                {"type": "tool.call", "tool": "demo.echo", "params": {"text": "hi"}}
            )
            assert frame.type == "tool.result"
            await asyncio.sleep(0.05)
            assert len(client.events) == 1
            assert client.events[0].event == "test.event"
            await client.stop()
            assert client.running is False
        finally:
            server.close()
            await server.wait_closed()

    asyncio.run(scenario())
