from __future__ import annotations

import asyncio

from builders import WhatsAppBuilders


class _FakeClient:
    def __init__(self):
        self.sent = []

    def send_message(self, channel_id, payload):
        self.sent.append((channel_id, payload))
        return {"ok": True}


def test_send_document_uses_pcap_mime_type():
    client = _FakeClient()
    builders = WhatsAppBuilders(client)
    asyncio.run(builders.send_document("1@s.whatsapp.net", "/tmp/capture.pcap"))
    _chat_id, payload = client.sent[0]
    assert payload["type"] == "document"
    assert payload["mime_type"] == "application/vnd.tcpdump.pcap"


def test_send_text_payload_shape():
    client = _FakeClient()
    builders = WhatsAppBuilders(client)
    asyncio.run(builders.send_text("1@s.whatsapp.net", "hello"))
    _chat_id, payload = client.sent[0]
    assert payload == {"type": "text", "text": "hello"}
