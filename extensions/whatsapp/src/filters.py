"""Inbound filtering for WhatsApp gateway traffic."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Iterable

from schema import OpenAgentMessage


@dataclass(slots=True)
class FilterConfig:
    process_groups: bool = True
    require_mention_in_groups: bool = True
    mention_aliases: list[str] = field(default_factory=list)


def is_group_chat(channel_id: str | None) -> bool:
    return bool(channel_id and channel_id.endswith("@g.us"))


def message_mentions_self(
    message: OpenAgentMessage,
    *,
    self_id: str | None = None,
    aliases: Iterable[str] = (),
) -> bool:
    if message.was_mentioned is True:
        return True
    mentioned = set(message.mentioned_jids or [])
    if self_id and self_id in mentioned:
        return True
    text = (message.body or "").lower()
    for alias in aliases:
        if alias and alias.lower() in text:
            return True
    return False


def should_process_message(
    message: OpenAgentMessage,
    config: FilterConfig,
    *,
    self_id: str | None = None,
) -> tuple[bool, str]:
    if not is_group_chat(message.channel_id):
        return True, "direct-chat"
    if not config.process_groups:
        return False, "group-disabled"
    if not config.require_mention_in_groups:
        return True, "group-open"
    if message_mentions_self(message, self_id=self_id, aliases=config.mention_aliases):
        return True, "group-mentioned"
    return False, "group-no-mention"
