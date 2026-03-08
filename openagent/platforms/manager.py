"""PlatformManager — dynamic adapter registry and outbound router.

Lifecycle
---------
1. ``start()`` launches two background tasks:
   - ``_monitor_services``: polls ServiceManager every 2 s, creates/removes
     platform adapters as services come online or go offline.
   - ``_route_outbound``: drains ``bus.outbound`` and dispatches each
     ``OutboundMessage`` to the correct ``PlatformAdapter``.

2. ``stop()`` cancels both tasks.

Service ↔ adapter mapping
--------------------------
The module-level ``_PLATFORM_ADAPTERS`` dict maps service names discovered in
``services/*/service.json`` to the corresponding ``PlatformAdapter`` subclass.
Extend it to add new platform services without touching this class.
"""

from __future__ import annotations

import asyncio
from typing import Any, Callable

from openagent.bus.bus import MessageBus
from openagent.observability.logging import get_logger
from openagent.services.manager import ServiceManager

from .adapter import (
    PlatformAdapter,
    DiscordPlatformAdapter,
    SlackPlatformAdapter,
    TelegramPlatformAdapter,
    WhatsAppPlatformAdapter,
)

logger = get_logger(__name__)

# service name → adapter class
_PLATFORM_ADAPTERS: dict[str, type[PlatformAdapter]] = {
    "discord": DiscordPlatformAdapter,
    "telegram": TelegramPlatformAdapter,
    "slack": SlackPlatformAdapter,
    "whatsapp": WhatsAppPlatformAdapter,
}

_MONITOR_INTERVAL_S = 2.0


class PlatformManager:
    """Manages platform adapters and routes outbound messages.

    Adapters are created automatically when the underlying Go service client
    becomes available in ``ServiceManager``, and removed when it goes offline.
    On service restart the adapter is rebuilt from the new client instance.
    """

    def __init__(
        self,
        *,
        service_manager: ServiceManager,
        bus: MessageBus,
        session_manager: object | None = None,
        get_connectors_enabled: Callable[[], dict[str, bool]] | None = None,
    ) -> None:
        self._service_manager = service_manager
        self._bus = bus
        # Bind SessionManager.resolve_user_key as the identity resolver for adapters.
        # When None, adapters fall back to the platform:channel_id session key.
        self._resolver = (
            session_manager.resolve_user_key if session_manager is not None else None
        )
        self._get_connectors_enabled = get_connectors_enabled or (lambda: {})
        # platform_name → adapter
        self._adapters: dict[str, PlatformAdapter] = {}
        self._route_task: asyncio.Task[None] | None = None
        self._monitor_task: asyncio.Task[None] | None = None

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def start(self) -> None:
        self._route_task = asyncio.create_task(
            self._route_outbound(), name="platformmgr.route"
        )
        self._monitor_task = asyncio.create_task(
            self._monitor_services(), name="platformmgr.monitor"
        )
        # Run an initial sync so adapters are ready immediately if services
        # are already online (e.g. when called after a short startup wait).
        self._sync_adapters()
        logger.info(
            "PlatformManager started — %d adapter(s) ready: %s",
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
        logger.info("PlatformManager stopped")

    # ------------------------------------------------------------------
    # Manual registration (optional override)
    # ------------------------------------------------------------------

    def register(self, adapter: PlatformAdapter) -> None:
        """Register a pre-built adapter.  Overrides auto-detected adapter."""
        self._adapters[adapter.platform_name] = adapter
        logger.info("PlatformManager: manually registered adapter for %r", adapter.platform_name)

    def adapters(self) -> dict[str, PlatformAdapter]:
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
                logger.error("PlatformManager monitor error: %s", exc)

    def _sync_adapters(self) -> None:
        """Create or remove adapters based on current service client state."""
        enabled_map = self._get_connectors_enabled()
        for svc_name, AdapterClass in _PLATFORM_ADAPTERS.items():
            # Skip disabled connectors (Settings > Connector tab)
            if enabled_map.get(svc_name) is False:
                existing = self._adapters.get(svc_name)
                if existing is not None:
                    del self._adapters[svc_name]
                    logger.info(
                        "PlatformManager: removed adapter for %r (connector disabled)", svc_name
                    )
                continue

            client = self._service_manager.get_client(svc_name)
            existing = self._adapters.get(svc_name)

            if client is None:
                if existing is not None:
                    del self._adapters[svc_name]
                    logger.info(
                        "PlatformManager: removed adapter for %r (service offline)", svc_name
                    )
                continue

            # Same client object — adapter is still valid.
            if existing is not None and existing.client is client:
                continue

            # New client (first start or restart) — create fresh adapter.
            self._adapters[svc_name] = AdapterClass(
                client=client,
                bus=self._bus,
                resolver=self._resolver,
            )
            logger.info("PlatformManager: attached adapter for %r", svc_name)

    # ------------------------------------------------------------------
    # Background: route outbound
    # ------------------------------------------------------------------

    async def _route_outbound(self) -> None:
        """Drain bus.outbound and dispatch each message to the right adapter."""
        while True:
            msg = await self._bus.outbound.get()
            if msg is None:
                return  # Shutdown signal from bus.close()

            adapter = self._adapters.get(msg.platform)
            if adapter is None:
                logger.warning(
                    "PlatformManager: no adapter for platform %r (active: %s)",
                    msg.platform,
                    list(self._adapters),
                )
                continue

            try:
                await adapter.send(msg)
            except Exception as exc:
                logger.error(
                    "PlatformManager: send failed for platform %r: %s", msg.platform, exc
                )
