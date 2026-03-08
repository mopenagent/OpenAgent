"""ToolRegistry — maps tool names to MCP-lite clients from ServiceManager.

The agent loop calls ``ToolRegistry.call(name, args)`` without knowing
which Go/Rust service owns the tool.  The registry handles dispatch.

Tool schemas are in OpenAI function-calling format so they can be passed
directly to ``provider.chat(tools=[...])`` regardless of provider.

Native tools
------------
Python-native tools (not backed by a Go service) can be registered via
``register_native(name, description, params_schema, fn)``.  Their handler
signature is ``async fn(session_key: str, args: dict) -> str``.  Native
registrations survive ``rebuild()`` calls.
"""

from __future__ import annotations

import json
import logging
from collections.abc import Awaitable, Callable
from typing import Any

from openagent.services.manager import ServiceManager
from openagent.services import protocol as proto

NativeHandler = Callable[[str, dict[str, Any]], Awaitable[str]]

logger = logging.getLogger(__name__)

_TOOL_CALL_TIMEOUT = 30.0  # seconds


class ToolRegistry:
    """Discovers tools from all running services and dispatches tool calls.

    Tools are registered per-service as each comes online via
    ``register_service()``.  The ``ServiceManager`` fires this callback
    from its watchdog whenever a service becomes ready, so tools appear
    incrementally rather than in one bulk startup call.

    Native Python tools (not backed by a Go service) survive service restarts.
    """

    def __init__(self, service_manager: ServiceManager) -> None:
        self._mgr = service_manager
        # tool_name → service_name for routing (Go service tools)
        self._tool_to_service: dict[str, str] = {}
        # Per-service schema lists — keyed by service name for easy replacement on restart
        self._service_schemas: dict[str, list[dict[str, Any]]] = {}
        # Native Python tools — survive service restarts
        self._native_handlers: dict[str, NativeHandler] = {}
        self._native_schemas: list[dict[str, Any]] = []

    # ------------------------------------------------------------------
    # Per-service registration (called by ServiceManager watchdog callback)
    # ------------------------------------------------------------------

    async def register_service(self, name: str) -> None:
        """Query tools from one service and register them.

        Called by ``ServiceManager.on_service_ready`` each time a service
        finishes starting (including after restarts).  Replaces any
        previously registered tools from the same service.
        """
        from openagent.platforms.mcplite import McpLiteClient  # avoid circular at module level
        client: McpLiteClient | None = self._mgr.get_client(name)
        if client is None:
            logger.warning("ToolRegistry: service %r ready but client not available", name)
            return

        try:
            frame = await client.request({"type": "tools.list"}, timeout_s=5.0)
        except Exception as exc:
            logger.warning("ToolRegistry: could not list tools from %r: %s", name, exc)
            return

        if not isinstance(frame, proto.ToolListResponse):
            logger.warning("ToolRegistry: unexpected response from %r tools.list", name)
            return

        # Remove old routing entries for this service
        self._tool_to_service = {
            t: s for t, s in self._tool_to_service.items() if s != name
        }

        schemas: list[dict[str, Any]] = []
        for tool in frame.tools:
            if tool.name in self._tool_to_service:
                logger.warning(
                    "Duplicate tool %r — service %r overrides %r",
                    tool.name, name, self._tool_to_service[tool.name],
                )
            self._tool_to_service[tool.name] = name
            schemas.append(_to_openai_schema(tool))

        self._service_schemas[name] = schemas
        logger.info(
            "ToolRegistry: registered %d tools from service %r (total service tools: %d)",
            len(schemas), name, sum(len(v) for v in self._service_schemas.values()),
        )

    async def rebuild(self) -> None:
        """Bootstrap: register tools from all currently-running services.

        Useful as a one-shot fallback when the service-ready callback was
        not set before services started.
        """
        for svc in self._mgr.list_services():
            await self.register_service(svc.name)

    # ------------------------------------------------------------------
    # Native tool registration
    # ------------------------------------------------------------------

    def register_native(
        self,
        name: str,
        description: str,
        params_schema: dict[str, Any],
        fn: NativeHandler,
    ) -> None:
        """Register a Python-native tool alongside Go service tools.

        ``fn`` must be ``async fn(session_key: str, args: dict) -> str``.
        Native registrations survive ``rebuild()`` calls.
        """
        self._native_handlers[name] = fn
        self._native_schemas.append({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": params_schema or {"type": "object", "properties": {}},
            },
        })
        logger.debug("ToolRegistry: registered native tool %r", name)

    # ------------------------------------------------------------------
    # Query
    # ------------------------------------------------------------------

    def schemas(self) -> list[dict[str, Any]]:
        """Return all tool schemas (Go services + native) in OpenAI format."""
        svc_schemas: list[dict[str, Any]] = []
        for s in self._service_schemas.values():
            svc_schemas.extend(s)
        return svc_schemas + list(self._native_schemas)

    def has_tools(self) -> bool:
        return bool(self._service_schemas) or bool(self._native_schemas)

    def has_service_tools(self) -> bool:
        """True when at least one Go/Rust service tool is registered."""
        return bool(self._service_schemas)

    def search(self, query: str) -> list[dict[str, Any]]:
        """Return schemas whose name or description match query (case-insensitive).

        When query is empty or ``"*"``, all tools except ``search_tools`` itself
        are returned — useful for the LLM to browse the full catalog.
        """
        q = query.lower().strip()
        results: list[dict[str, Any]] = []

        def _matches(schema: dict[str, Any]) -> bool:
            fn = schema.get("function", {})
            name = fn.get("name", "").lower()
            if name == "search_tools":  # never return search_tools in its own results
                return False
            if not q or q == "*":
                return True
            desc = fn.get("description", "").lower()
            return any(w in name or w in desc for w in q.split())

        for schemas in self._service_schemas.values():
            results.extend(s for s in schemas if _matches(s))
        results.extend(s for s in self._native_schemas if _matches(s))
        return results

    # ------------------------------------------------------------------
    # Dispatch
    # ------------------------------------------------------------------

    async def call(
        self,
        name: str,
        arguments: dict[str, Any],
        *,
        session_key: str = "",
    ) -> str:
        """Invoke a tool — native Python tools first, then Go service tools.

        Returns the result string.  On error returns an error description
        instead of raising — the agent loop injects this into the conversation
        so the LLM can react gracefully.
        """
        # Native tools take priority (no network hop)
        if name in self._native_handlers:
            try:
                return await self._native_handlers[name](session_key, arguments)
            except Exception as exc:
                msg = f"[tool error] {name!r}: {exc}"
                logger.error(msg)
                return msg

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
