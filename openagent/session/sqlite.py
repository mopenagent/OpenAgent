"""SQLite session backend — aiosqlite, no ORM, atomic writes."""

from __future__ import annotations

import logging
from datetime import datetime
from pathlib import Path
from typing import Literal

import aiosqlite

from .backend import Turn

logger = logging.getLogger(__name__)

_CREATE_SQL = """
CREATE TABLE IF NOT EXISTS turns (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_key TEXT    NOT NULL,
    role        TEXT    NOT NULL,
    content     TEXT    NOT NULL,
    tool_call_id TEXT   NOT NULL DEFAULT '',
    tool_name   TEXT    NOT NULL DEFAULT '',
    ts          TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_turns_session ON turns (session_key, id);
"""


class SqliteSessionBackend:
    """Async SQLite backend using aiosqlite.

    All writes use WAL journal mode for concurrency and fsync safety.
    When the Go session service is ready, swap this for GoSessionBackend —
    the SessionManager constructor is the only change required.
    """

    def __init__(self, db_path: Path | str) -> None:
        self._db_path = Path(db_path)
        self._db: aiosqlite.Connection | None = None

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def start(self) -> None:
        self._db_path.parent.mkdir(parents=True, exist_ok=True)
        self._db = await aiosqlite.connect(str(self._db_path))
        self._db.row_factory = aiosqlite.Row
        await self._db.executescript(_CREATE_SQL)
        await self._db.execute("PRAGMA journal_mode=WAL")
        await self._db.commit()
        logger.debug("SqliteSessionBackend opened %s", self._db_path)

    async def stop(self) -> None:
        if self._db:
            await self._db.close()
            self._db = None

    # ------------------------------------------------------------------
    # SessionBackend interface
    # ------------------------------------------------------------------

    async def append(
        self,
        session_key: str,
        role: Literal["system", "user", "assistant", "tool"],
        content: str,
        *,
        tool_call_id: str = "",
        tool_name: str = "",
    ) -> None:
        assert self._db, "backend not started"
        ts = datetime.now().isoformat()
        await self._db.execute(
            "INSERT INTO turns (session_key, role, content, tool_call_id, tool_name, ts)"
            " VALUES (?, ?, ?, ?, ?, ?)",
            (session_key, role, content, tool_call_id, tool_name, ts),
        )
        await self._db.commit()

    async def get_history(
        self, session_key: str, *, limit: int = 100
    ) -> list[Turn]:
        assert self._db, "backend not started"
        async with self._db.execute(
            "SELECT role, content, tool_call_id, tool_name, ts FROM turns"
            " WHERE session_key = ?"
            " ORDER BY id DESC LIMIT ?",
            (session_key, limit),
        ) as cursor:
            rows = await cursor.fetchall()
        # Reverse so oldest-first
        return [
            Turn(
                role=r["role"],
                content=r["content"],
                tool_call_id=r["tool_call_id"],
                tool_name=r["tool_name"],
                timestamp=datetime.fromisoformat(r["ts"]),
            )
            for r in reversed(rows)
        ]

    async def set_summary(self, session_key: str, summary: str) -> None:
        """Atomically replace all turns with a single system summary."""
        assert self._db, "backend not started"
        ts = datetime.now().isoformat()
        async with self._db.execute("BEGIN"):
            await self._db.execute(
                "DELETE FROM turns WHERE session_key = ?", (session_key,)
            )
            await self._db.execute(
                "INSERT INTO turns (session_key, role, content, tool_call_id, tool_name, ts)"
                " VALUES (?, 'system', ?, '', '', ?)",
                (session_key, f"[Summary] {summary}", ts),
            )
        await self._db.commit()

    async def clear(self, session_key: str) -> None:
        assert self._db, "backend not started"
        await self._db.execute(
            "DELETE FROM turns WHERE session_key = ?", (session_key,)
        )
        await self._db.commit()

    async def list_sessions(self) -> list[str]:
        assert self._db, "backend not started"
        async with self._db.execute(
            "SELECT DISTINCT session_key FROM turns ORDER BY session_key"
        ) as cursor:
            rows = await cursor.fetchall()
        return [r["session_key"] for r in rows]
