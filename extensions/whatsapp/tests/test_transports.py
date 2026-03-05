from __future__ import annotations

import asyncio
import json
from pathlib import Path

from transports import NeonizeWhatsAppTransport, ServiceWhatsAppTransport


async def _wait_until(predicate, timeout: float = 2.0) -> None:
    end = asyncio.get_running_loop().time() + timeout
    while asyncio.get_running_loop().time() < end:
        if predicate():
            return
        await asyncio.sleep(0.02)
    raise TimeoutError("condition was not met before timeout")


class _FakeNeonizeClient:
    def __init__(self, on_qr=None, on_event=None):
        self._on_qr = on_qr
        self._on_event = on_event
        self._connected = False

    def connect(self):
        self._connected = True
        if self._on_qr:
            self._on_qr("qr://fake")
        if self._on_event:
            self._on_event({"type": "ConnectedEv", "status": "connected", "self_id": "111@s.whatsapp.net"})

    def disconnect(self):
        self._connected = False

    def is_connected(self):
        return self._connected

    def send_message(self, channel_id, payload):
        return {"channel_id": channel_id, "payload": payload}

    def emit_message(self, event):
        if self._on_event:
            self._on_event(event)


def test_neonize_transport_smoke(monkeypatch, tmp_path: Path):
    created = {}

    def fake_create_client(self, *, on_qr=None, on_event=None):
        client = _FakeNeonizeClient(on_qr=on_qr, on_event=on_event)
        created["client"] = client
        return client

    monkeypatch.setattr(
        "transports.SessionManager.create_client",
        fake_create_client,
    )

    async def scenario():
        transport = NeonizeWhatsAppTransport(data_dir=tmp_path, account_id="default")
        await transport.start()

        await _wait_until(lambda: transport.latest_qr() is not None)
        await _wait_until(lambda: bool(transport.get_status().get("connected")))

        client = created["client"]
        client.emit_message(
            {
                "type": "MessageEv",
                "channel_id": "123@s.whatsapp.net",
                "body": "hello from test",
            }
        )
        await asyncio.sleep(0.1)
        messages = transport.pop_messages()
        assert len(messages) == 1

        result = await transport.send_text("123@s.whatsapp.net", "hello")
        assert result["chat_id"] == "123@s.whatsapp.net"
        assert result["payload"]["text"] == "hello"

        await transport.stop()
        assert transport.get_status()["running"] is False

    asyncio.run(scenario())


def test_service_transport_smoke(tmp_path: Path):
    socket_path = tmp_path / "whatsapp.sock"

    async def handle(reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
        while True:
            line = await reader.readline()
            if not line:
                break
            request = json.loads(line.decode("utf-8"))
            if request["type"] == "tools.list":
                tools_resp = {
                    "id": request["id"],
                    "type": "tools.list.ok",
                    "tools": [
                        {
                            "name": "whatsapp.send_text",
                            "description": "Send text",
                            "params": {"type": "object", "properties": {}},
                        },
                        {
                            "name": "whatsapp.status",
                            "description": "Status",
                            "params": {"type": "object", "properties": {}},
                        },
                    ],
                }
                writer.write((json.dumps(tools_resp) + "\n").encode("utf-8"))
                writer.write(
                    (
                        json.dumps(
                            {
                                "type": "event",
                                "event": "whatsapp.connection.status",
                                "data": {"connected": True, "backend": "service"},
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
                                "event": "whatsapp.qr",
                                "data": {"qr": "service://qr"},
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
                                "event": "whatsapp.message.received",
                                "data": {
                                    "id": "m1",
                                    "channel_id": "123@s.whatsapp.net",
                                    "from_id": "123@s.whatsapp.net",
                                    "body": "hello from service",
                                },
                            }
                        )
                        + "\n"
                    ).encode("utf-8")
                )
                await writer.drain()
            elif request["type"] == "tool.call":
                tool_resp = {
                    "id": request["id"],
                    "type": "tool.result",
                    "result": "ok",
                    "error": None,
                }
                writer.write((json.dumps(tool_resp) + "\n").encode("utf-8"))
                await writer.drain()
        writer.close()
        await writer.wait_closed()

    async def scenario():
        server = await asyncio.start_unix_server(handle, path=str(socket_path))
        try:
            transport = ServiceWhatsAppTransport(socket_path=socket_path)
            await transport.start()

            await _wait_until(lambda: transport.latest_qr() == "service://qr")
            await _wait_until(lambda: bool(transport.get_status().get("connected")))
            await asyncio.sleep(0.1)
            messages = transport.pop_messages()
            assert len(messages) == 1

            result = await transport.send_text("123@s.whatsapp.net", "hello")
            assert result == "ok"

            await transport.stop()
            assert transport.get_status()["running"] is False
        finally:
            server.close()
            await server.wait_closed()

    asyncio.run(scenario())
