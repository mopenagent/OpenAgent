from __future__ import annotations

import asyncio

import plugin as plugin_module


class _FakeTransport:
    def __init__(self, backend: str):
        self.backend = backend
        self.started = False
        self.messages = [{"body": "hi"}]
        self.qr = f"{backend}://qr"

    async def start(self) -> None:
        self.started = True

    async def stop(self) -> None:
        self.started = False

    async def send_text(self, channel_id: str, text: str):
        return {"backend": self.backend, "channel_id": channel_id, "text": text}

    def get_status(self):
        return {"backend": self.backend, "running": self.started, "connected": self.started}

    def latest_qr(self):
        return self.qr

    def pop_messages(self):
        batch = list(self.messages)
        self.messages.clear()
        return batch


def test_plugin_uses_configured_backend(monkeypatch):
    neonize = _FakeTransport("neonize")
    service = _FakeTransport("service")
    monkeypatch.setattr(plugin_module, "NeonizeWhatsAppTransport", lambda **_kwargs: neonize)
    monkeypatch.setattr(plugin_module, "ServiceWhatsAppTransport", lambda **_kwargs: service)

    async def scenario(backend: str):
        ext = plugin_module.WhatsAppExtension(backend=backend)
        await ext.initialize()
        assert ext.get_status()["backend"] == backend
        assert ext.latest_qr() == f"{backend}://qr"
        assert len(ext.pop_messages()) == 1
        result = await ext.send_text("123", "hello")
        assert result["backend"] == backend
        await ext.shutdown()
        assert ext.get_status()["running"] is False

    asyncio.run(scenario("neonize"))
    asyncio.run(scenario("service"))
