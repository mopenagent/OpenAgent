from __future__ import annotations

import asyncio

from filters import FilterConfig
from handlers import WhatsAppHandlers
from heartbeat import HeartbeatTracker


def test_handler_filters_group_noise_without_mention():
    seen = []
    tracker = HeartbeatTracker()
    handler = WhatsAppHandlers(
        heartbeat=tracker,
        filter_config=FilterConfig(require_mention_in_groups=True),
        self_id_getter=lambda: "999@s.whatsapp.net",
        on_message=seen.append,
    )
    event = {"type": "MessageEv", "chat_id": "1-2@g.us", "body": "noise"}
    message = asyncio.run(handler.handle_event(event))
    assert message is None
    assert seen == []


def test_handler_processes_mentioned_group_message():
    seen = []
    tracker = HeartbeatTracker()
    handler = WhatsAppHandlers(
        heartbeat=tracker,
        filter_config=FilterConfig(require_mention_in_groups=True),
        self_id_getter=lambda: "999@s.whatsapp.net",
        on_message=seen.append,
    )
    event = {
        "type": "MessageEv",
        "chat_id": "1-2@g.us",
        "body": "hey",
        "mentioned_jids": ["999@s.whatsapp.net"],
    }
    message = asyncio.run(handler.handle_event(event))
    assert message is not None
    assert len(seen) == 1
