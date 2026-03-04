"""SessionManager — wraps a SessionBackend with auto-summarisation.

Usage::

    backend = SqliteSessionBackend(db_path=root / "data" / "sessions.db")
    mgr = SessionManager(backend=backend, summarise_after=20)
    await mgr.start()

    # Agent loop:
    history = await mgr.get_history(session_key)
    await mgr.append(session_key, "user", msg.content)
    await mgr.append(session_key, "assistant", reply)

    # On shutdown:
    await mgr.stop()

Go/Rust migration
-----------------
Replace the constructor argument::

    # now
    backend = SqliteSessionBackend(db_path=...)
    # later — Go session service registered via ServiceManager
    backend = GoSessionBackend(socket_path=root / "data" / "sockets" / "session.sock")

No other code changes required.
"""

from __future__ import annotations

import logging
from typing import Any, Callable, Awaitable, Literal

from openagent.providers.base import Message

from .backend import SessionBackend, Turn

logger = logging.getLogger(__name__)

# Sentinel passed to the summarise callback so it can call the LLM
SummariseCallback = Callable[[list[Turn]], Awaitable[str]]


class SessionManager:
    """High-level session API with optional auto-summarisation.

    Parameters
    ----------
    backend:
        Any ``SessionBackend`` implementation (SQLite now, Go/Rust later).
    summarise_after:
        Number of turns after which auto-summarisation fires.
        Set to 0 to disable.
    summarise_fn:
        Async callable ``(turns) -> summary_str`` — called when threshold is
        hit.  Typically calls the LLM provider.  Required when
        ``summarise_after > 0``.
    """

    def __init__(
        self,
        backend: SessionBackend,
        *,
        summarise_after: int = 40,
        summarise_fn: SummariseCallback | None = None,
    ) -> None:
        self._backend = backend
        self._summarise_after = summarise_after
        self._summarise_fn = summarise_fn

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def start(self) -> None:
        await self._backend.start()
        logger.debug("SessionManager started")

    async def stop(self) -> None:
        await self._backend.stop()
        logger.debug("SessionManager stopped")

    # ------------------------------------------------------------------
    # Core API
    # ------------------------------------------------------------------

    async def get_history(
        self, session_key: str, *, limit: int = 100
    ) -> list[Turn]:
        """Return up to ``limit`` turns, oldest first."""
        return await self._backend.get_history(session_key, limit=limit)

    async def append(
        self,
        session_key: str,
        role: Literal["system", "user", "assistant", "tool"],
        content: str,
        *,
        tool_call_id: str = "",
        tool_name: str = "",
    ) -> None:
        """Append a turn and trigger summarisation if the threshold is reached."""
        await self._backend.append(
            session_key, role, content,
            tool_call_id=tool_call_id,
            tool_name=tool_name,
        )
        if self._summarise_after > 0:
            await self._maybe_summarise(session_key)

    async def clear(self, session_key: str) -> None:
        await self._backend.clear(session_key)

    async def list_sessions(self) -> list[str]:
        return await self._backend.list_sessions()

    # ------------------------------------------------------------------
    # History → provider.Message conversion
    # ------------------------------------------------------------------

    def to_messages(self, history: list[Turn]) -> list[Message]:
        """Convert Turn objects to provider Message objects for LLM input."""
        return [
            Message(
                role=t.role,
                content=t.content,
                tool_call_id=t.tool_call_id,
                tool_name=t.tool_name,
            )
            for t in history
        ]

    # ------------------------------------------------------------------
    # Auto-summarisation
    # ------------------------------------------------------------------

    async def _maybe_summarise(self, session_key: str) -> None:
        turns = await self._backend.get_history(session_key, limit=self._summarise_after + 5)
        if len(turns) < self._summarise_after:
            return
        if not self._summarise_fn:
            logger.warning(
                "summarise_after=%d reached for %s but no summarise_fn configured",
                self._summarise_after,
                session_key,
            )
            return
        logger.info(
            "Auto-summarising session %s (%d turns)", session_key, len(turns)
        )
        try:
            summary = await self._summarise_fn(turns)
            await self._backend.set_summary(session_key, summary)
            logger.info("Session %s summarised", session_key)
        except Exception:
            logger.exception("Failed to summarise session %s", session_key)
