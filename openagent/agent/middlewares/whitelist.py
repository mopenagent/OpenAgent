"""Whitelist middleware — blocks inbound messages not in the allowed list."""

from __future__ import annotations

import asyncio

from openagent.bus.events import InboundMessage
from openagent.observability.logging import get_logger

logger = get_logger(__name__)


class WhitelistMiddleware:
    """Inbound middleware that gates messages against an in-memory whitelist.

    The whitelist is loaded from the SessionBackend on start() and can be
    refreshed at any time by calling refresh().
    """

    direction = "inbound"

    def __init__(self, backend) -> None:
        self._backend = backend
        self._allowed: set[tuple[str, str]] = set()  # (platform, channel_id)
        self._lock = asyncio.Lock()

    async def start(self) -> None:
        await self.refresh()

    async def refresh(self) -> None:
        entries = await self._backend.get_whitelist()
        async with self._lock:
            self._allowed = {(e["platform"], e["channel_id"]) for e in entries}
        logger.info("Whitelist loaded: %d entries", len(self._allowed))

    async def __call__(self, msg: InboundMessage) -> None:
        # Log incoming message JID before whitelist check (for debugging)
        logger.info(
            "Inbound before whitelist: platform=%s channel_id=%s (JID)",
            msg.platform,
            msg.channel_id,
        )
        async with self._lock:
            allowed = (msg.platform, msg.channel_id) in self._allowed
        if not allowed:
            logger.info(
                "Whitelist blocked — JID=%s (add to Settings → Whitelist: %s:%s)",
                msg.channel_id,
                msg.platform,
                msg.channel_id,
            )
            msg.metadata["_blocked"] = True
            if hasattr(self._backend, "record_seen_sender"):
                try:
                    await self._backend.record_seen_sender(msg.platform, msg.channel_id)
                except Exception:
                    pass
        else:
            logger.debug("Whitelist allowed %s:%s", msg.platform, msg.channel_id)
