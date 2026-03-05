"""Web platform adapter — bridges /ws/chat WebSocket connections to the MessageBus.

Not backed by a Go service.  Pure Python.  Each browser tab that connects
to /ws/chat calls ``register_connection()`` with a unique ``channel_id`` and
an async send callback.  When the agent loop dispatches an OutboundMessage
with ``platform="web"``, platformManager calls ``adapter.send(msg)`` which
routes to the correct WebSocket.

Registration lifecycle
----------------------
1. WS connects  → ``register_connection(channel_id, send_fn)``
2. User message → ``bus.publish(InboundMessage(platform="web", channel_id=...))``
3. Agent reply  → ``bus.dispatch(OutboundMessage(platform="web", channel_id=...))``
4. platformManager → ``adapter.send(msg)`` → ``send_fn(msg.content)``
5. WS disconnects → ``unregister_connection(channel_id)``
"""

from __future__ import annotations

from collections.abc import Callable, Coroutine
from typing import Any

from openagent.bus.events import OutboundMessage
from openagent.observability.logging import get_logger

logger = get_logger(__name__)

# Type alias: async callable(content, stream_chunk=False) for delivering replies.
SendFn = Callable[..., Coroutine[Any, Any, None]]


class WebPlatformAdapter:
    """Routes OutboundMessage → active WebSocket send callbacks.

    Registered with platformManager via ``platform_manager.register(adapter)``
    so the platformManager can dispatch ``platform="web"`` messages to it.
    """

    platform_name: str = "web"

    def __init__(self) -> None:
        self._connections: dict[str, SendFn] = {}

    # ------------------------------------------------------------------
    # Connection registry
    # ------------------------------------------------------------------

    def register_connection(self, channel_id: str, send_fn: SendFn) -> None:
        """Called by the WebSocket handler on connect."""
        self._connections[channel_id] = send_fn
        logger.info("Webplatform: registered — channel_id=%r  active=%d",
                    channel_id, len(self._connections))

    def unregister_connection(self, channel_id: str) -> None:
        """Called by the WebSocket handler on disconnect."""
        self._connections.pop(channel_id, None)
        logger.info("Webplatform: removed — channel_id=%r  active=%d",
                    channel_id, len(self._connections))

    def active_connections(self) -> list[str]:
        return list(self._connections)

    # ------------------------------------------------------------------
    # platformManager interface
    # ------------------------------------------------------------------

    async def send(self, msg: OutboundMessage) -> None:
        """Deliver msg.content to the WebSocket identified by msg.channel_id."""
        send_fn = self._connections.get(msg.channel_id)
        if send_fn is None:
            logger.warning("Webplatform: no connection for channel_id=%r", msg.channel_id)
            return
        try:
            meta = msg.metadata or {}
            stream_chunk = meta.get("stream_chunk", False)
            if stream_chunk:
                await send_fn(msg.content, stream_chunk=True)
            else:
                await send_fn(msg.content)
        except Exception as exc:
            logger.error("Webplatform: send error channel_id=%r: %s", msg.channel_id, exc)
            self._connections.pop(msg.channel_id, None)
