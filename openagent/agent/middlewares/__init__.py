"""Middleware interfaces for the agent loop.

A middleware is a simple async callable with a ``direction`` attribute that
tells the loop which chain to put it in:

    direction="inbound"  — runs before the LLM; mutates InboundMessage in-place
    direction="outbound" — runs after  the LLM; mutates OutboundMessage in-place

Both directions share the same Protocol so one class can satisfy either role
just by setting its ``direction`` attribute.  No NextCall / chain-of-responsibility
needed — the loop owns the two flat chains.

Example::

    class LoggingMiddleware:
        direction = "inbound"

        async def __call__(self, msg: InboundMessage) -> None:
            logger.debug("inbound: %s", msg.content[:80])

    class UpperCaseMiddleware:
        direction = "outbound"

        async def __call__(self, msg: OutboundMessage) -> None:
            msg.content = msg.content.upper()
"""

from __future__ import annotations

from typing import Literal, Protocol, Union

from openagent.bus.events import InboundMessage, OutboundMessage
from openagent.agent.middlewares.whitelist import WhitelistMiddleware

AnyMessage = Union[InboundMessage, OutboundMessage]

__all__ = ["AgentMiddleware", "WhitelistMiddleware"]


class AgentMiddleware(Protocol):
    """Middleware that processes a message in-place.

    ``direction`` controls which chain the loop inserts this middleware into:

    * ``"inbound"``  — called with the ``InboundMessage`` before the LLM.
    * ``"outbound"`` — called with the ``OutboundMessage`` after the LLM.

    Mutations to ``msg`` are visible to downstream middlewares and the
    final dispatch.  Raising an exception skips the remaining chain and
    logs a warning; it does not crash the session worker.
    """

    direction: Literal["inbound", "outbound"]

    async def __call__(self, msg: AnyMessage) -> None:
        """Process ``msg`` in-place.  Return value is ignored."""
        ...
