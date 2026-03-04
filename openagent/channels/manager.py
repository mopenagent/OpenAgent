"""ChannelManager — dynamic adapter registry and outbound router.

Lifecycle
---------
1. ``start()`` launches two background tasks:
   - ``_monitor_services``: polls ServiceManager every 2 s, creates/removes
     channel adapters as services come online or go offline.
   - ``_route_outbound``: drains ``bus.outbound`` and dispatches each
     ``OutboundMessage`` to the correct ``ChannelAdapter``.

2. ``stop()`` cancels both tasks.

Service ↔ adapter mapping
--------------------------
The module-level ``_CHANNEL_ADAPTERS`` dict maps service names discovered in
``services/*/service.json`` to the corresponding ``ChannelAdapter`` subclass.
Extend it to add new channel services without touching this class.
"""

from __future__ import annotations

import asyncio
from typing import Any

from openagent.bus.bus import MessageBus
from openagent.observability.logging import get_logger
from openagent.services.manager import ServiceManager

from .adapter import (
    ChannelAdapter,
    DiscordChannelAdapter,
    SlackChannelAdapter,
    TelegramChannelAdapter,
)

logger = get_logger(__name__)

# service name → adapter class
_CHANNEL_ADAPTERS: dict[str, type[ChannelAdapter]] = {
    "discord": DiscordChannelAdapter,
    "telegram": TelegramChannelAdapter,
    "slack": SlackChannelAdapter,
}

_MONITOR_INTERVAL_S = 2.0


class ChannelManager:
    """Manages channel adapters and routes outbound messages.

    Adapters are created automatically when the underlying Go service client
    becomes available in ``ServiceManager``, and removed when it goes offline.
    On service restart the adapter is rebuilt from the new client instance.
    """

    def __init__(self, *, service_manager: ServiceManager, bus: MessageBus) -> None:
        self._service_manager = service_manager
        self._bus = bus
        # channel_name → adapter
        self._adapters: dict[str, ChannelAdapter] = {}
        self._route_task: asyncio.Task[None] | None = None
        self._monitor_task: asyncio.Task[None] | None = None

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def start(self) -> None:
        self._route_task = asyncio.create_task(
            self._route_outbound(), name="channelmgr.route"
        )
        self._monitor_task = asyncio.create_task(
            self._monitor_services(), name="channelmgr.monitor"
        )
        # Run an initial sync so adapters are ready immediately if services
        # are already online (e.g. when called after a short startup wait).
        self._sync_adapters()
        logger.info(
            "ChannelManager started — %d adapter(s) ready: %s",
            len(self._adapters),
            list(self._adapters),
        )

    async def stop(self) -> None:
        for task in (self._monitor_task, self._route_task):
            if task and not task.done():
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
        self._monitor_task = None
        self._route_task = None
        logger.info("ChannelManager stopped")

    # ------------------------------------------------------------------
    # Manual registration (optional override)
    # ------------------------------------------------------------------

    def register(self, adapter: ChannelAdapter) -> None:
        """Register a pre-built adapter.  Overrides auto-detected adapter."""
        self._adapters[adapter.channel_name] = adapter
        logger.info("ChannelManager: manually registered adapter for %r", adapter.channel_name)

    def adapters(self) -> dict[str, ChannelAdapter]:
        return dict(self._adapters)

    # ------------------------------------------------------------------
    # Background: monitor
    # ------------------------------------------------------------------

    async def _monitor_services(self) -> None:
        while True:
            try:
                await asyncio.sleep(_MONITOR_INTERVAL_S)
                self._sync_adapters()
            except asyncio.CancelledError:
                return
            except Exception as exc:
                logger.error("ChannelManager monitor error: %s", exc)

    def _sync_adapters(self) -> None:
        """Create or remove adapters based on current service client state."""
        for svc_name, AdapterClass in _CHANNEL_ADAPTERS.items():
            client = self._service_manager.get_client(svc_name)
            existing = self._adapters.get(svc_name)

            if client is None:
                if existing is not None:
                    del self._adapters[svc_name]
                    logger.info(
                        "ChannelManager: removed adapter for %r (service offline)", svc_name
                    )
                continue

            # Same client object — adapter is still valid.
            if existing is not None and existing.client is client:
                continue

            # New client (first start or restart) — create fresh adapter.
            self._adapters[svc_name] = AdapterClass(client=client, bus=self._bus)
            logger.info("ChannelManager: attached adapter for %r", svc_name)

    # ------------------------------------------------------------------
    # Background: route outbound
    # ------------------------------------------------------------------

    async def _route_outbound(self) -> None:
        """Drain bus.outbound and dispatch each message to the right adapter."""
        while True:
            msg = await self._bus.outbound.get()
            if msg is None:
                return  # Shutdown signal from bus.close()

            adapter = self._adapters.get(msg.channel)
            if adapter is None:
                logger.warning(
                    "ChannelManager: no adapter for channel %r (active: %s)",
                    msg.channel,
                    list(self._adapters),
                )
                continue

            try:
                await adapter.send(msg)
            except Exception as exc:
                logger.error(
                    "ChannelManager: send failed for channel %r: %s", msg.channel, exc
                )
