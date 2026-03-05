from __future__ import annotations

from gateway import GatewayConfig, WhatsAppGateway
from handlers import WhatsAppHandlers
from heartbeat import HeartbeatTracker
from filters import FilterConfig


class _FakeClient:
    def __init__(self):
        self.connected = False

    def connect(self):
        self.connected = True

    def disconnect(self):
        self.connected = False

    def is_connected(self):
        return self.connected

    def send_message(self, channel_id, payload):
        return {"channel_id": channel_id, "payload": payload}


class _FakeSession:
    def __init__(self):
        self.client = _FakeClient()

    def is_linked(self):
        return True

    def auth_age_ms(self):
        return 10

    def read_self_id(self):
        return "111@s.whatsapp.net"

    def create_client(self, **_kwargs):
        return self.client


def test_gateway_start_and_stop_updates_heartbeat():
    heartbeat = HeartbeatTracker()
    handlers = WhatsAppHandlers(
        heartbeat=heartbeat,
        filter_config=FilterConfig(),
        on_message=lambda _msg: None,
    )
    gateway = WhatsAppGateway(
        session=_FakeSession(),
        handlers=handlers,
        heartbeat=heartbeat,
        config=GatewayConfig(reconnect_base_seconds=0.01, reconnect_max_seconds=0.01),
    )
    gateway.start()
    gateway.stop()
    snap = heartbeat.snapshot()
    assert snap.running is False
