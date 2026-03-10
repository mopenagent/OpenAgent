"""Persistent key-value settings store backed by the shared openagent.db SQLite file."""

from __future__ import annotations

from pathlib import Path

import aiosqlite


class SettingsStore:
    """Async key-value store in the `settings` table of openagent.db.

    Keys are plain strings (e.g. ``"connector.slack.enabled"``).
    Values are stored as TEXT; callers convert to the appropriate type.

    The store opens its own connection to the DB file.  SQLite handles
    concurrent access from the SessionManager's connection via WAL journal.
    """

    def __init__(self, db_path: Path) -> None:
        self._path = db_path
        self._db: aiosqlite.Connection | None = None

    async def start(self) -> None:
        self._db = await aiosqlite.connect(self._path)
        self._db.row_factory = aiosqlite.Row
        await self._db.execute("PRAGMA journal_mode=WAL")
        await self._db.execute("""
            CREATE TABLE IF NOT EXISTS settings (
                key        TEXT PRIMARY KEY,
                value      TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
        """)
        # Persists operator intent (start/stop) and lifecycle timestamps per service.
        # enabled=1 means the ServiceManager should run it; 0 means keep it stopped.
        await self._db.execute("""
            CREATE TABLE IF NOT EXISTS service_state (
                name          TEXT PRIMARY KEY,
                enabled       INTEGER NOT NULL DEFAULT 1,
                last_started  TEXT,
                last_stopped  TEXT,
                last_error    TEXT,
                restart_count INTEGER NOT NULL DEFAULT 0,
                updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
            )
        """)
        await self._db.commit()

    async def stop(self) -> None:
        if self._db:
            await self._db.close()
            self._db = None

    async def get(self, key: str, default: str = "") -> str:
        assert self._db, "SettingsStore not started"
        async with self._db.execute(
            "SELECT value FROM settings WHERE key = ?", (key,)
        ) as cur:
            row = await cur.fetchone()
        return row[0] if row else default

    async def set(self, key: str, value: str) -> None:
        assert self._db, "SettingsStore not started"
        await self._db.execute(
            """
            INSERT INTO settings (key, value, updated_at)
            VALUES (?, ?, datetime('now'))
            ON CONFLICT(key) DO UPDATE
                SET value = excluded.value,
                    updated_at = excluded.updated_at
            """,
            (key, value),
        )
        await self._db.commit()

    # ---------------------------------------------------------------------------
    # service_state table — lifecycle persistence
    # ---------------------------------------------------------------------------

    async def get_all_service_states(self) -> dict[str, dict]:
        """Return all rows from service_state keyed by service name."""
        assert self._db, "SettingsStore not started"
        async with self._db.execute(
            "SELECT name, enabled, last_started, last_stopped, last_error, restart_count"
            " FROM service_state"
        ) as cur:
            rows = await cur.fetchall()
        return {
            row[0]: {
                "enabled": bool(row[1]),
                "last_started": row[2],
                "last_stopped": row[3],
                "last_error": row[4],
                "restart_count": row[5],
            }
            for row in rows
        }

    async def set_service_enabled(self, name: str, enabled: bool) -> None:
        """Persist operator intent: should ServiceManager run this service?"""
        assert self._db, "SettingsStore not started"
        await self._db.execute(
            """
            INSERT INTO service_state (name, enabled, updated_at)
            VALUES (?, ?, datetime('now'))
            ON CONFLICT(name) DO UPDATE
                SET enabled = excluded.enabled,
                    updated_at = excluded.updated_at
            """,
            (name, 1 if enabled else 0),
        )
        await self._db.commit()

    async def record_service_start(self, name: str) -> None:
        """Record a successful service start event."""
        assert self._db, "SettingsStore not started"
        await self._db.execute(
            """
            INSERT INTO service_state (name, enabled, last_started, last_error, updated_at)
            VALUES (?, 1, datetime('now'), NULL, datetime('now'))
            ON CONFLICT(name) DO UPDATE
                SET last_started = datetime('now'),
                    last_error   = NULL,
                    updated_at   = datetime('now')
            """,
            (name,),
        )
        await self._db.commit()

    async def record_service_stop(self, name: str, error: str | None = None) -> None:
        """Record a service stop event (clean shutdown or crash)."""
        assert self._db, "SettingsStore not started"
        await self._db.execute(
            """
            INSERT INTO service_state (name, enabled, last_stopped, last_error, updated_at)
            VALUES (?, 1, datetime('now'), ?, datetime('now'))
            ON CONFLICT(name) DO UPDATE
                SET last_stopped = datetime('now'),
                    last_error   = CASE WHEN ? IS NOT NULL THEN ? ELSE last_error END,
                    updated_at   = datetime('now')
            """,
            (name, error, error, error),
        )
        await self._db.commit()

    async def record_service_restart(self, name: str) -> None:
        """Increment the restart counter for a service (watchdog-triggered)."""
        assert self._db, "SettingsStore not started"
        await self._db.execute(
            """
            INSERT INTO service_state (name, enabled, restart_count, updated_at)
            VALUES (?, 1, 1, datetime('now'))
            ON CONFLICT(name) DO UPDATE
                SET restart_count = restart_count + 1,
                    updated_at    = datetime('now')
            """,
            (name,),
        )
        await self._db.commit()

    # ---------------------------------------------------------------------------
    # Key-value settings
    # ---------------------------------------------------------------------------

    async def get_all(self, prefix: str = "") -> dict[str, str]:
        assert self._db, "SettingsStore not started"
        if prefix:
            async with self._db.execute(
                "SELECT key, value FROM settings WHERE key LIKE ?",
                (f"{prefix}%",),
            ) as cur:
                rows = await cur.fetchall()
        else:
            async with self._db.execute("SELECT key, value FROM settings") as cur:
                rows = await cur.fetchall()
        return {row[0]: row[1] for row in rows}
