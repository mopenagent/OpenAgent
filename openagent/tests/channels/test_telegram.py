from __future__ import annotations

import asyncio
import json
from pathlib import Path

from openagent.channels.telegram import TelegramServiceChannel


def test_telegram_service_channel_flow(tmp_path: Path):
    socket_path = Path("/tmp/oa_test_telegram.sock")

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
                            {"id": req["id"], "type": "tools.list.ok", "tools": []}
                        )
                        + "\n"
                    ).encode("utf-8")
                )
                writer.write(
                    (
                        json.dumps(
                            {
                                "type": "event",
                                "event": "telegram.connection.status",
                                "data": {"connected": True, "authorized": True},
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
                                "event": "telegram.message.received",
                                "data": {"id": "t1", "text": "ping"},
                            }
                        )
                        + "\n"
                    ).encode("utf-8")
                )
            elif req["tool"] == "telegram.status":
                writer.write(
                    (
                        json.dumps(
                            {
                                "id": req["id"],
                                "type": "tool.result",
                                "result": json.dumps(
                                    {
                                        "running": True,
                                        "connected": True,
                                        "authorized": True,
                                        "backend": "gotd.td",
                                    }
                                ),
                                "error": None,
                            }
                        )
                        + "\n"
                    ).encode("utf-8")
                )
            elif req["tool"] == "telegram.link_state":
                writer.write(
                    (
                        json.dumps(
                            {
                                "id": req["id"],
                                "type": "tool.result",
                                "result": json.dumps(
                                    {
                                        "connected": True,
                                        "authorized": True,
                                    }
                                ),
                                "error": None,
                            }
                        )
                        + "\n"
                    ).encode("utf-8")
                )
            elif req["tool"] == "telegram.send_message":
                writer.write(
                    (
                        json.dumps(
                            {
                                "id": req["id"],
                                "type": "tool.result",
                                "result": json.dumps({"ok": True}),
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
        server = await asyncio.start_unix_server(handler, path=str(socket_path))
        try:
            channel = TelegramServiceChannel(socket_path=socket_path)
            await channel.start()
            await asyncio.sleep(0.05)
            status = await channel.get_status()
            assert status["connected"] is True
            link = await channel.get_link_state()
            assert link["authorized"] is True
            sent = await channel.send_message(user_id=1, access_hash=2, text="hello")
            assert sent["ok"] is True
            messages = channel.pop_messages()
            assert len(messages) == 1
            assert messages[0]["text"] == "ping"
            await channel.stop()
        finally:
            server.close()
            await server.wait_closed()

    asyncio.run(scenario())
