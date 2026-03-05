from __future__ import annotations

from filters import FilterConfig, should_process_message
from schema import OpenAgentMessage


def test_direct_messages_are_allowed():
    message = OpenAgentMessage(channel_id="1555@s.whatsapp.net", body="hello", chat_type="direct")
    allowed, reason = should_process_message(message, FilterConfig())
    assert allowed is True
    assert reason == "direct-chat"


def test_group_requires_mention_by_default():
    message = OpenAgentMessage(channel_id="1-2@g.us", body="hello team", chat_type="group")
    allowed, reason = should_process_message(message, FilterConfig(), self_id="999@s.whatsapp.net")
    assert allowed is False
    assert reason == "group-no-mention"


def test_group_message_with_mention_is_allowed():
    message = OpenAgentMessage(
        channel_id="1-2@g.us",
        body="hello @bot",
        chat_type="group",
        mentioned_jids=["999@s.whatsapp.net"],
    )
    allowed, reason = should_process_message(
        message, FilterConfig(), self_id="999@s.whatsapp.net"
    )
    assert allowed is True
    assert reason == "group-mentioned"
