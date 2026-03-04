"""ToolRegistry — maps tool names to MCP-lite clients from ServiceManager.

The agent loop calls ``ToolRegistry.call(name, args)`` without knowing
which Go/Rust service owns the tool.  The registry handles dispatch.

Tool schemas are in OpenAI function-calling format so they can be passed
directly to ``provider.chat(tools=[...])`` regardless of provider.
"""

from __future__ import annotations

import json
import logging
from typing import Any

from openagent.services.manager import ServiceManager
from openagent.services import protocol as proto

logger = logging.getLogger(__name__)

_TOOL_CALL_TIMEOUT = 30.0  # seconds


class ToolRegistry:
    """Discovers tools from all running services and dispatches tool calls.

    Built once per ``AgentLoop`` from the ``ServiceManager``.  Refreshed on
    ``rebuild()`` if services restart and expose new tools.
    """

    def __init__(self, service_manager: ServiceManager) -> None:
        self._mgr = service_manager
        # tool_name → service_name for routing
        self._tool_to_service: dict[str, str] = {}
        # OpenAI-format schemas for the LLM
        self._schemas: list[dict[str, Any]] = []

    # ------------------------------------------------------------------
    # Build / rebuild
    # ------------------------------------------------------------------

    async def rebuild(self) -> None:
        """Discover all tools currently exposed by running services.

        Called at startup and whenever a service restarts (watchdog can
        notify the agent loop to rebuild).
        """
        self._tool_to_service.clear()
        self._schemas.clear()

        for svc in self._mgr.list_services():
            client = self._mgr.get_client(svc.name)
            if client is None:
                continue
            try:
                frame = await client.request({"type": "tools.list"}, timeout_s=5.0)
            except Exception:
                logger.warning("Could not list tools from service %s", svc.name)
                continue

            if not isinstance(frame, proto.ToolListResponse):
                continue

            for tool in frame.tools:
                if tool.name in self._tool_to_service:
                    logger.warning(
                        "Duplicate tool name %r — service %s overrides %s",
                        tool.name, svc.name, self._tool_to_service[tool.name],
                    )
                self._tool_to_service[tool.name] = svc.name
                self._schemas.append(_to_openai_schema(tool))

        logger.info(
            "ToolRegistry: %d tools from %d services",
            len(self._schemas),
            len({s for s in self._tool_to_service.values()}),
        )

    # ------------------------------------------------------------------
    # Query
    # ------------------------------------------------------------------

    def schemas(self) -> list[dict[str, Any]]:
        """Return tool schemas in OpenAI function-calling format."""
        return list(self._schemas)

    def has_tools(self) -> bool:
        return bool(self._schemas)

    # ------------------------------------------------------------------
    # Dispatch
    # ------------------------------------------------------------------

    async def call(self, name: str, arguments: dict[str, Any]) -> str:
        """Invoke a tool on its owning service.

        Returns the result string.  On error returns an error description
        instead of raising — the agent loop injects this into the conversation
        so the LLM can react gracefully.
        """
        service_name = self._tool_to_service.get(name)
        if service_name is None:
            msg = f"[tool error] unknown tool: {name!r}"
            logger.warning(msg)
            return msg

        client = self._mgr.get_client(service_name)
        if client is None:
            msg = f"[tool error] service {service_name!r} not running"
            logger.warning(msg)
            return msg

        try:
            frame = await client.request(
                {"type": "tool.call", "tool": name, "params": arguments},
                timeout_s=_TOOL_CALL_TIMEOUT,
            )
        except TimeoutError:
            msg = f"[tool error] {name!r} timed out after {_TOOL_CALL_TIMEOUT}s"
            logger.error(msg)
            return msg
        except Exception as exc:
            msg = f"[tool error] {name!r}: {exc}"
            logger.error(msg)
            return msg

        if isinstance(frame, proto.ToolResultResponse):
            if frame.error:
                return f"[tool error] {frame.error}"
            return frame.result or ""

        return f"[tool error] unexpected frame type: {type(frame).__name__}"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _to_openai_schema(tool: proto.ToolDefinition) -> dict[str, Any]:
    """Convert a MCP-lite ToolDefinition to OpenAI function-calling format."""
    return {
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.params or {"type": "object", "properties": {}},
        },
    }
