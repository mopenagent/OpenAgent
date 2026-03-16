"""DiaryStore — reads conversation history from cortex diary markdown files.

Diary files are written by the Rust cortex service at:
  data/diary/{session_key}/{unix_timestamp}.md

Markdown format:
  # Session: {session_key}
  **Timestamp:** {ts}
  ## User input
  {content}
  ## Response
  {content}
  ## Tools used
  {tool list or _none_}

Contact name resolution (priority order):
  1. data/contacts.json  — operator-set names keyed by session_key
  2. whitelist.label     — label set when whitelisting a contact
  3. Formatted session_key — "WhatsApp: 916356737267" etc.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

import aiosqlite


@dataclass
class DiaryMessage:
    role: str       # "user" or "assistant"
    content: str
    timestamp: int  # unix seconds


@dataclass
class SessionInfo:
    key: str
    display_name: str
    platform: str
    channel_id: str
    last_active: int   # unix timestamp of most recent diary entry
    message_count: int


class DiaryStore:
    """Reads conversation history from cortex diary markdown files."""

    def __init__(self, diary_root: Path, db_path: Path):
        self.diary_root = diary_root
        self.db_path = db_path
        self._contacts_path = diary_root.parent / "contacts.json"
        self._contacts: dict[str, str] = {}
        self._load_contacts()

    # ------------------------------------------------------------------
    # Contact name management
    # ------------------------------------------------------------------

    def _load_contacts(self) -> None:
        if self._contacts_path.exists():
            try:
                self._contacts = json.loads(self._contacts_path.read_text())
            except Exception:
                self._contacts = {}

    def set_contact_name(self, session_key: str, name: str) -> None:
        """Persist a human-readable name for a session."""
        self._contacts[session_key] = name
        self._contacts_path.write_text(
            json.dumps(self._contacts, indent=2, ensure_ascii=False)
        )

    async def _whitelist_label(self, platform: str, channel_id: str) -> Optional[str]:
        """Look up whitelist.label for (platform, channel_id)."""
        try:
            async with aiosqlite.connect(self.db_path) as db:
                async with db.execute(
                    "SELECT label FROM whitelist WHERE platform=? AND channel_id=? AND label != ''",
                    (platform, channel_id),
                ) as cur:
                    row = await cur.fetchone()
                    return row[0] if row else None
        except Exception:
            return None

    async def resolve_display_name(self, session_key: str) -> str:
        """Resolve a human-readable display name for a session key."""
        # 1. Operator-set name
        if session_key in self._contacts:
            return self._contacts[session_key]

        # 2. Whitelist label
        platform, channel_id = parse_session_key(session_key)
        if platform and channel_id:
            label = await self._whitelist_label(platform, channel_id)
            if label:
                return label

        # 3. Formatted fallback
        return format_session_key(session_key)

    # ------------------------------------------------------------------
    # History reading
    # ------------------------------------------------------------------

    def get_history(self, session_key: str) -> list[DiaryMessage]:
        """Read all diary entries for a session key, sorted by timestamp."""
        session_dir = self.diary_root / session_key
        if not session_dir.exists():
            return []

        messages: list[DiaryMessage] = []
        for md_file in sorted(session_dir.glob("*.md")):
            try:
                ts = int(md_file.stem)
            except ValueError:
                continue
            parsed = _parse_diary_md(md_file.read_text())
            if not parsed:
                continue
            user_in, response = parsed
            if user_in:
                messages.append(DiaryMessage("user", user_in, ts))
            if response:
                messages.append(DiaryMessage("assistant", response, ts))

        return messages

    # ------------------------------------------------------------------
    # Session listing
    # ------------------------------------------------------------------

    async def _hidden_keys(self) -> set[str]:
        """Return session keys that have been soft-deleted."""
        try:
            async with aiosqlite.connect(self.db_path) as db:
                async with db.execute(
                    "SELECT session_key FROM session_metadata WHERE hidden_at IS NOT NULL"
                ) as cur:
                    rows = await cur.fetchall()
                    return {r[0] for r in rows}
        except Exception:
            return set()

    async def hide_session(self, session_key: str) -> None:
        """Mark a session as hidden in session_metadata."""
        try:
            async with aiosqlite.connect(self.db_path) as db:
                await db.execute(
                    """INSERT INTO session_metadata (session_key, hidden_at)
                       VALUES (?, datetime('now'))
                       ON CONFLICT(session_key) DO UPDATE SET hidden_at = datetime('now')""",
                    (session_key,),
                )
                await db.commit()
        except Exception:
            pass

    async def list_sessions(self) -> list[SessionInfo]:
        """List all visible sessions from diary directories, newest first."""
        if not self.diary_root.exists():
            return []

        hidden = await self._hidden_keys()
        sessions: list[SessionInfo] = []

        for session_dir in self.diary_root.iterdir():
            if not session_dir.is_dir():
                continue
            key = session_dir.name
            if key in hidden:
                continue

            md_files = sorted(session_dir.glob("*.md"))
            if not md_files:
                continue

            last_ts = 0
            count = 0
            for f in md_files:
                try:
                    ts = int(f.stem)
                    last_ts = max(last_ts, ts)
                    count += 1
                except ValueError:
                    pass

            platform, channel_id = parse_session_key(key)
            display_name = await self.resolve_display_name(key)

            sessions.append(SessionInfo(
                key=key,
                display_name=display_name,
                platform=platform or "unknown",
                channel_id=channel_id or key,
                last_active=last_ts,
                message_count=count,
            ))

        sessions.sort(key=lambda s: s.last_active, reverse=True)
        return sessions


# ------------------------------------------------------------------
# Parsing helpers (also imported by routes)
# ------------------------------------------------------------------

def _parse_diary_md(text: str) -> Optional[tuple[str, str]]:
    """Parse a diary markdown file into (user_input, response)."""
    user_input = ""
    response = ""

    m = re.search(r"## User input\s*\n(.*?)(?=\n## |\Z)", text, re.DOTALL)
    if m:
        user_input = m.group(1).strip()

    m = re.search(r"## Response\s*\n(.*?)(?=\n## |\Z)", text, re.DOTALL)
    if m:
        response = m.group(1).strip()

    if not user_input and not response:
        return None
    return user_input, response


def parse_session_key(key: str) -> tuple[Optional[str], Optional[str]]:
    """Extract (platform, sender_id) from a session key.

    session_key format from dispatch.rs: "{channel}:{sender}"
    where channel = "platform://chatID"

    Examples:
      'web:abc123'                              → ('web', 'abc123')
      'user:uuid'                               → ('user', 'uuid')
      'whatsapp://chatID@lid:senderID@lid'      → ('whatsapp', 'senderID@lid')
      'whatsapp://chatID@net:senderID@net'      → ('whatsapp', 'senderID@net')
      'discord://guild/channel:senderID'        → ('discord', 'senderID')

    For whitelist lookup, the sender_id is used as channel_id (this is what
    dispatch.rs passes to guard.check as {"channel_id": sender}).
    """
    if "://" in key:
        platform = key.split("://")[0]
        rest = key.split("://", 1)[1]  # chatID:senderID (or just chatID)

        # chatID@domain:senderID → split on the ':' between chatID and senderID.
        # chatID always contains '@' (it's a JID). senderID also contains '@'.
        # For '52922670915662@lid:52922670915662@lid':
        #   parts = ['52922670915662@lid', '52922670915662@lid']
        # For '916356737267@s.whatsapp.net:916356737267@s.whatsapp.net':
        #   parts = ['916356737267@s.whatsapp.net', '916356737267@s.whatsapp.net']
        parts = rest.split(":")
        if len(parts) >= 2 and "@" in parts[0]:
            # sender is everything after the first chatID segment
            sender = ":".join(parts[1:])
            return platform, sender
        return platform, rest

    if ":" in key:
        platform, channel_id = key.split(":", 1)
        return platform, channel_id

    return None, None


def format_session_key(key: str) -> str:
    """Human-readable fallback label when no name is configured."""
    platform, channel_id = parse_session_key(key)

    if platform == "web":
        short = (channel_id or "")[:8]
        return f"Web ({short})"

    if platform == "user":
        short = (channel_id or "")[:8]
        return f"User ({short})"

    if platform and channel_id:
        # Strip @domain for display: '916356737267@s.whatsapp.net' → '916356737267'
        short = channel_id.split("@")[0]
        return f"{platform.capitalize()}: {short}"

    return key[:24]
