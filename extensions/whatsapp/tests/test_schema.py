from __future__ import annotations

from schema import from_neonize_message_event


def test_message_event_conversion_with_group_fields():
    event = {
        "id": "m-1",
        "chat_id": "12345-67890@g.us",
        "body": "hello @bot",
        "sender_jid": "111@s.whatsapp.net",
        "sender_name": "Alice",
        "mentioned_jids": ["999@s.whatsapp.net"],
        "timestamp": 1700000000,
        "media_type": "image/png",
        "media_path": "/tmp/image.png",
    }
    message = from_neonize_message_event(event, account_id="personal")
    assert message is not None
    assert message.id == "m-1"
    assert message.chat_type == "group"
    assert message.account_id == "personal"
    assert message.media_path == "/tmp/image.png"
    assert message.timestamp == 1700000000000


def test_message_conversion_returns_none_for_empty_payload():
    assert from_neonize_message_event({}) is None
    assert from_neonize_message_event({"chat_id": "x"}) is None
