"""OpenAgent-compatible message schemas for WhatsApp events."""

from __future__ import annotations

from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from typing import Any


def _read(source: Any, *keys: str) -> Any:
    for key in keys:
        if isinstance(source, dict) and key in source:
            return source[key]
        if hasattr(source, key):
            return getattr(source, key)
    return None


def _to_timestamp_ms(value: Any) -> int | None:
    if value is None:
        return None
    if isinstance(value, datetime):
        return int(value.timestamp() * 1000)
    if isinstance(value, (int, float)):
        # Heuristic: values below year ~2001 in ms are likely unix seconds.
        if value < 1_000_000_000_000:
            return int(value * 1000)
        return int(value)
    return None


@dataclass(slots=True)
class OpenAgentMessage:
    """Normalized inbound message envelope for OpenAgent."""

    id: str | None = None
    from_id: str | None = None
    to_id: str | None = None
    account_id: str = "default"
    body: str = ""
    timestamp: int | None = None
    chat_type: str = "direct"
    channel_id: str | None = None
    sender_jid: str | None = None
    sender_e164: str | None = None
    sender_name: str | None = None
    reply_to_id: str | None = None
    reply_to_body: str | None = None
    reply_to_sender: str | None = None
    reply_to_sender_jid: str | None = None
    reply_to_sender_e164: str | None = None
    group_subject: str | None = None
    group_participants: list[str] | None = None
    mentioned_jids: list[str] | None = None
    self_jid: str | None = None
    self_e164: str | None = None
    from_me: bool = False
    media_path: str | None = None
    media_type: str | None = None
    media_file_name: str | None = None
    media_url: str | None = None
    was_mentioned: bool | None = None
    body_for_agent: str | None = None
    body_for_commands: str | None = None
    command_authorized: bool | None = None
    conversation_label: str | None = None
    provider: str = "whatsapp"
    surface: str = "whatsapp"
    originating_channel: str = "whatsapp"
    originating_to: str | None = None
    media_paths: list[str] | None = None
    media_types: list[str] | None = None
    raw_event: dict[str, Any] = field(default_factory=dict)

    def to_context_dict(self) -> dict[str, Any]:
        """Convert to OpenClaw-like context keys for future parity."""
        return {
            "Body": self.body,
            "BodyForAgent": self.body_for_agent or self.body,
            "BodyForCommands": self.body_for_commands or self.body,
            "From": self.from_id,
            "To": self.to_id,
            "AccountId": self.account_id,
            "MessageSid": self.id,
            "ReplyToId": self.reply_to_id,
            "ReplyToBody": self.reply_to_body,
            "ReplyToSender": self.reply_to_sender,
            "ChatType": self.chat_type,
            "ConversationLabel": self.conversation_label or self.from_id,
            "GroupSubject": self.group_subject,
            "GroupMembers": ", ".join(self.group_participants or []),
            "SenderName": self.sender_name,
            "SenderId": self.sender_jid or self.sender_e164,
            "SenderE164": self.sender_e164,
            "Timestamp": self.timestamp,
            "MediaPath": self.media_path,
            "MediaType": self.media_type,
            "MediaPaths": self.media_paths,
            "MediaTypes": self.media_types,
            "Provider": self.provider,
            "Surface": self.surface,
            "WasMentioned": self.was_mentioned,
            "CommandAuthorized": bool(self.command_authorized),
            "OriginatingChannel": self.originating_channel,
            "OriginatingTo": self.originating_to or self.from_id,
        }


def from_neonize_message_event(event: Any, *, account_id: str = "default") -> OpenAgentMessage | None:
    """Best-effort conversion from Neonize MessageEv payload to OpenAgentMessage."""
    payload = event if isinstance(event, dict) else getattr(event, "__dict__", {})
    if not payload:
        return None

    text = _read(payload, "text", "body", "message", "caption")
    media_type = _read(payload, "media_type", "mime_type", "mimetype")
    media_path = _read(payload, "media_path", "file_path")
    media_url = _read(payload, "media_url", "url")
    body = str(text or "").strip()
    if not body and not media_type and not media_path and not media_url:
        return None

    channel_id = _read(payload, "chat_id", "chat_jid", "remote_jid", "conversation_id", "from")
    sender_jid = _read(payload, "sender_jid", "participant", "author")
    mentioned_jids = _read(payload, "mentioned_jids", "mentions") or None
    if isinstance(mentioned_jids, tuple):
        mentioned_jids = list(mentioned_jids)
    if mentioned_jids is not None and not isinstance(mentioned_jids, list):
        mentioned_jids = [str(mentioned_jids)]

    message = OpenAgentMessage(
        id=_read(payload, "id", "message_id", "msg_id"),
        from_id=str(channel_id) if channel_id is not None else None,
        to_id=_read(payload, "to", "to_id", "self_jid"),
        account_id=account_id,
        body=body,
        timestamp=_to_timestamp_ms(_read(payload, "timestamp", "ts", "time", "message_time")),
        chat_type="group" if isinstance(channel_id, str) and channel_id.endswith("@g.us") else "direct",
        channel_id=str(channel_id) if channel_id is not None else None,
        sender_jid=sender_jid,
        sender_e164=_read(payload, "sender_e164", "phone"),
        sender_name=_read(payload, "sender_name", "push_name", "name"),
        reply_to_id=_read(payload, "reply_to_id", "quoted_id"),
        reply_to_body=_read(payload, "reply_to_body", "quoted_text"),
        reply_to_sender=_read(payload, "reply_to_sender", "quoted_sender"),
        reply_to_sender_jid=_read(payload, "reply_to_sender_jid", "quoted_sender_jid"),
        reply_to_sender_e164=_read(payload, "reply_to_sender_e164", "quoted_sender_e164"),
        group_subject=_read(payload, "group_subject"),
        group_participants=_read(payload, "group_participants"),
        mentioned_jids=mentioned_jids,
        self_jid=_read(payload, "self_jid"),
        self_e164=_read(payload, "self_e164"),
        from_me=bool(_read(payload, "from_me", "is_from_me")),
        media_path=media_path,
        media_type=media_type,
        media_file_name=_read(payload, "media_file_name", "filename"),
        media_url=media_url,
        was_mentioned=_read(payload, "was_mentioned"),
        body_for_agent=_read(payload, "body_for_agent"),
        body_for_commands=_read(payload, "body_for_commands"),
        command_authorized=_read(payload, "command_authorized"),
        conversation_label=_read(payload, "conversation_label"),
        originating_to=str(channel_id) if channel_id is not None else None,
        media_paths=[media_path] if media_path else None,
        media_types=[media_type] if media_type else None,
        raw_event={k: v for k, v in payload.items()} if isinstance(payload, dict) else {},
    )
    return message
