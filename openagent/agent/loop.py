"""AgentLoop — custom ReAct loop. No framework dependency.

Architecture
------------
The loop reads from the MessageBus per-session queue, runs one full
ReAct iteration (LLM → tool calls → LLM → ... → final reply), saves turns
to the SessionManager, then dispatches the reply via the bus.

One coroutine per active session runs concurrently; they share the provider
and tool registry but each owns its own history slice.

Middleware
----------
Middlewares are split into two flat chains based on ``direction``:

* ``"inbound"``  — run before the LLM; receive and mutate ``InboundMessage``
* ``"outbound"`` — run after  the LLM; receive and mutate ``OutboundMessage``

Each middleware is a simple async callable that modifies the message in-place.
The loop owns the chaining; no NextCall / chain-of-responsibility needed.

Iteration limit
---------------
``MAX_ITERATIONS = 10`` prevents infinite loops when a model keeps calling
tools without converging.  When the limit is hit the loop returns whatever
partial content the model last produced (or a timeout notice).

Tool output truncation
----------------------
``MAX_TOOL_OUTPUT = 500`` characters.  Keeps the context window bounded on
low-RAM hardware (Pi 5 8 GB).  The model sees the truncated result; the full
output is logged at DEBUG level.

Session key
-----------
``InboundMessage.session_key`` is used throughout — it already handles
cross-platform identity (user_key → "user:<hex>") and per-chat fallback.

Abort
-----
``abort_session(session_key)`` cancels an in-progress loop for a given
session.  The abort is checked at the top of each iteration so the loop
exits cleanly between tool rounds.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
from typing import Any

from openagent.bus.bus import MessageBus
from openagent.bus.events import InboundMessage, OutboundMessage
from openagent.providers.base import LLMResponse, Message, Provider, StreamEvent, ToolCall
from openagent.session.manager import SessionManager
from openagent.agent.tools import ToolRegistry
from openagent.agent.middlewares import AgentMiddleware

logger = logging.getLogger(__name__)

MAX_ITERATIONS = 10        # default dropped from 40 — tunable per instance
MAX_TOOL_OUTPUT = 500      # chars; truncate beyond this
_SYSTEM_PROMPT = (
    "You are a helpful assistant. "
    "Use tools only when necessary. "
    "Be concise."
)


class AgentLoop:
    """Orchestrates InboundMessage → LLM → tools → OutboundMessage.

    Parameters
    ----------
    bus:        MessageBus — publish/subscribe point for all platforms.
    provider:   LLM provider implementing ``stream_with_tools(messages, tools)``.
    sessions:   SessionManager — history persistence.
    tools:      ToolRegistry — maps tool names to Go/Rust services.
    system_prompt:
        Override the default system prompt.
    middlewares:
        List of ``AgentMiddleware`` instances.  Split by ``direction``:
        ``"inbound"`` runs before the LLM; ``"outbound"`` runs after.
    """

    def __init__(
        self,
        bus: MessageBus,
        provider: Provider,
        sessions: SessionManager,
        tools: ToolRegistry,
        *,
        system_prompt: str = _SYSTEM_PROMPT,
        max_iterations: int = MAX_ITERATIONS,
        max_tool_output: int = MAX_TOOL_OUTPUT,
        middlewares: list[AgentMiddleware] | None = None,
    ) -> None:
        self._bus = bus
        self._provider = provider
        self._sessions = sessions
        self._tools = tools
        self._system_prompt = system_prompt
        self._max_iterations = max_iterations
        self._max_tool_output = max_tool_output
        mw = middlewares or []
        self._inbound_mw  = [m for m in mw if m.direction == "inbound"]
        self._outbound_mw = [m for m in mw if m.direction == "outbound"]
        self._tasks: dict[str, asyncio.Task[None]] = {}
        self._running = False
        # Abort signals — one Event per active session_key
        self._abort_events: dict[str, asyncio.Event] = {}

        # Register search_tools as a native meta-tool so the LLM can discover
        # available tools progressively rather than receiving the full catalog.
        self._search_tools_schema: dict[str, Any] = {
            "type": "function",
            "function": {
                "name": "search_tools",
                "description": (
                    "Discover available tools by keyword. "
                    "Call this first to find the right tool for your task. "
                    "Use an empty string to list all tools."
                ),
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Keyword(s) to match tool names/descriptions. Use '' for all.",
                        }
                    },
                    "required": ["query"],
                },
            },
        }

        async def _search_tools_handler(_session_key: str, args: dict[str, Any]) -> str:
            query = args.get("query", "")
            found = self._tools.search(query)
            return json.dumps(found) if found else "[]"

        self._tools.register_native(
            name="search_tools",
            description=(
                "Discover available tools by keyword. "
                "Call this first to find the right tool for your task."
            ),
            params_schema={
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Keyword(s) to match tool names/descriptions. Use '' for all.",
                    }
                },
                "required": ["query"],
            },
            fn=_search_tools_handler,
        )

    # ------------------------------------------------------------------
    # Lifecycle
    # ------------------------------------------------------------------

    async def start(self) -> None:
        """Start the agent loop — register session callback with the bus."""
        self._running = True
        self._bus.on_new_session(self._on_new_session)
        logger.info("AgentLoop started")

    async def stop(self) -> None:
        """Cancel all per-session tasks and wait for them to finish."""
        self._running = False
        for task in self._tasks.values():
            task.cancel()
        if self._tasks:
            await asyncio.gather(*self._tasks.values(), return_exceptions=True)
        self._tasks.clear()
        logger.info("AgentLoop stopped")

    def abort_session(self, session_key: str) -> None:
        """Cancel an in-progress agent loop for the given session.

        The running iteration will notice the abort flag at the top of its
        next loop cycle and break cleanly.  Safe to call when no loop is
        running — the event is cleaned up after the loop exits.
        """
        event = self._abort_events.setdefault(session_key, asyncio.Event())
        event.set()
        logger.info("Abort requested for session %s", session_key)

    # ------------------------------------------------------------------
    # Session dispatch
    # ------------------------------------------------------------------

    def _on_new_session(self, session_key: str) -> None:
        """Called by the bus fanout the first time a session_key appears."""
        if not self._running:
            return
        task = asyncio.create_task(
            self._session_worker(session_key),
            name=f"agent.session.{session_key}",
        )
        self._tasks[session_key] = task
        task.add_done_callback(lambda _: self._tasks.pop(session_key, None))

    async def _session_worker(self, session_key: str) -> None:
        """Drain the per-session queue; process each message sequentially."""
        try:
            q = self._bus.session_queue(session_key)
            while self._running:
                msg = await q.get()
                if msg is None:  # shutdown sentinel
                    break
                try:
                    outbound = await self._process(msg)
                    if outbound is not None:
                        await self._bus.dispatch(outbound)
                except asyncio.CancelledError:
                    raise
                except Exception:
                    logger.exception(
                        "Unhandled error processing message from session %s", session_key
                    )
        finally:
            self._abort_events.pop(session_key, None)

    # ------------------------------------------------------------------
    # Deterministic Middle — clean tool output before LLM sees it
    # ------------------------------------------------------------------

    def _process_tool_output(self, raw: str, tool_name: str) -> str:
        """Normalise raw tool output: JSON pretty-print → strip ANSI → collapse
        whitespace → truncate.  Applied to every tool result before it enters
        the message history (the "Deterministic Middle" pattern).
        """
        # 1. Pretty-print if valid JSON
        try:
            parsed = json.loads(raw)
            raw = json.dumps(parsed, indent=2, ensure_ascii=False)
        except (ValueError, TypeError):
            pass

        # 2. Strip ANSI escape codes
        raw = re.sub(r"\x1b\[[0-9;]*[mGKHF]", "", raw)

        # 3. Normalise whitespace — collapse tabs/spaces; collapse 3+ blank lines
        raw = re.sub(r"[ \t]+", " ", raw)
        raw = re.sub(r"\n{3,}", "\n\n", raw)
        raw = raw.strip()

        # 4. Truncate — keep context window bounded on low-RAM hardware
        if len(raw) > self._max_tool_output:
            logger.debug(
                "Truncating tool output for %s: %d → %d chars",
                tool_name, len(raw), self._max_tool_output,
            )
            raw = raw[: self._max_tool_output] + "…[truncated]"

        return raw

    # ------------------------------------------------------------------
    # Tool execution helper (Rec 1 + Rec 3: extract + per-tool isolation)
    # ------------------------------------------------------------------

    async def _execute_tool_calls(
        self,
        tool_calls: list[ToolCall],
        messages: list[Message],
        session_key: str,
    ) -> dict[str, str]:
        """Execute tool calls, append results to messages and session.

        Returns a mapping of tool-name → processed-result so the caller can
        inspect results (e.g. to expand active_schemas from search_tools).

        Each tool call is isolated: an exception from one tool injects an
        error string into the message history so the LLM can self-correct
        rather than crashing the entire loop iteration.
        """
        results: dict[str, str] = {}
        for tc in tool_calls:
            logger.debug("Tool call: %s(%s)", tc.name, tc.arguments)
            try:
                raw_result = await self._tools.call(
                    tc.name, tc.arguments, session_key=session_key
                )
            except Exception as exc:
                # Per-tool error isolation — inject error, LLM can self-correct
                raw_result = f"[tool error] {tc.name}: {exc}"
                logger.warning("Tool %s raised: %s", tc.name, exc)
            processed = self._process_tool_output(raw_result, tc.name)
            results[tc.name] = processed
            await self._sessions.append(
                session_key, "tool", processed,
                tool_call_id=tc.id,
                tool_name=tc.name,
            )
            messages.append(Message(
                "tool", processed,
                tool_call_id=tc.id,
                tool_name=tc.name,
            ))
        return results

    # ------------------------------------------------------------------
    # Middleware chains + core ReAct
    # ------------------------------------------------------------------

    async def _process(self, msg: InboundMessage) -> OutboundMessage | None:
        """Run inbound chain → ReAct → outbound chain.

        ``_run_react`` always returns a plain string (final_content).
        Outbound middleware and final dispatch happen here — never inside
        ``_run_react`` — so the loop boundary is clean (Rec 8).

        Returns ``None`` because the final ``OutboundMessage`` is dispatched
        directly here; the session worker's dispatch call is a no-op.
        """
        # ── Inbound chain ──────────────────────────────────────────────
        for mw in self._inbound_mw:
            try:
                await mw(msg)
            except Exception:
                logger.exception("Inbound middleware %s raised", type(mw).__name__)

        # ── Whitelist / middleware block check ─────────────────────────
        if msg.metadata.get("_blocked"):
            logger.info("Message blocked by middleware — skipping agent loop")
            return None

        # ── ReAct loop — returns plain string (Rec 8) ──────────────────
        final_content = await self._run_react(msg)

        # ── Save assistant turn ────────────────────────────────────────
        session_key = msg.session_key
        if final_content:
            await self._sessions.append(session_key, "assistant", final_content)

        # ── Build final outbound message ───────────────────────────────
        outbound = OutboundMessage(
            platform=msg.platform,
            channel_id=msg.channel_id,
            content=final_content,
            session_key=session_key,
            metadata={**dict(msg.metadata), "stream_chunk": True, "stream_end": True},
        )

        # ── Outbound middleware chain (Rec 8) ──────────────────────────
        for mw in self._outbound_mw:
            try:
                await mw(outbound)
            except Exception:
                logger.exception("Outbound middleware %s raised", type(mw).__name__)

        # ── Final dispatch ─────────────────────────────────────────────
        await self._bus.dispatch(outbound)
        return None  # already dispatched; session_worker does nothing

    async def _run_react(self, msg: InboundMessage) -> str:
        """Run one full ReAct turn using stream_with_tools (Rec 5).

        Always returns the final accumulated text content.  Intermediate
        streaming chunks are dispatched inline; the stream_end / outbound
        middleware are the caller's responsibility (Rec 8).

        The abort signal is checked at the top of each iteration (Rec 7).
        """
        session_key = msg.session_key

        # Save user turn
        await self._sessions.append(session_key, "user", msg.content)

        # Build message list: system + history
        history = await self._sessions.get_history(session_key)
        messages: list[Message] = [
            Message("system", self._system_prompt),
            *self._sessions.to_messages(history),
        ]

        # Per-turn ephemeral tool set — starts with only search_tools when service
        # tools exist so the LLM discovers them via search_tools().  Discarded
        # when this coroutine returns (never bleeds across turns).
        if self._tools.has_service_tools():
            active_schemas: list[dict[str, Any]] | None = [self._search_tools_schema]
        elif self._tools.has_tools():
            active_schemas = self._tools.schemas()
        else:
            active_schemas = None

        final_content = ""
        iteration = 0

        try:
            while True:
                # Hard backstop — limit as last resort, not primary exit
                if iteration >= self._max_iterations:
                    logger.warning(
                        "Session %s hit max_iterations=%d; returning partial reply",
                        session_key, self._max_iterations,
                    )
                    if not final_content:
                        final_content = "[max iterations reached — partial response]"
                    break

                # Abort check (Rec 7)
                if self._abort_events.get(session_key, asyncio.Event()).is_set():
                    logger.info(
                        "Session %s aborted at iteration %d", session_key, iteration
                    )
                    break

                accumulated = ""
                response_tool_calls: list[ToolCall] = []

                try:
                    # Single code path — stream_with_tools always (Rec 5)
                    async for event in self._provider.stream_with_tools(
                        messages, tools=active_schemas
                    ):
                        if not isinstance(event, StreamEvent):
                            continue
                        if event.content:
                            accumulated += event.content
                            # Dispatch every chunk unconditionally (Rec 2)
                            await self._bus.dispatch(OutboundMessage(
                                platform=msg.platform,
                                channel_id=msg.channel_id,
                                content=accumulated,
                                session_key=session_key,
                                metadata={
                                    **dict(msg.metadata),
                                    "stream_chunk": True,
                                    "stream_end": False,
                                },
                            ))
                        if event.tool_calls:
                            response_tool_calls = event.tool_calls
                except Exception as exc:
                    logger.error(
                        "LLM stream failed on iteration %d: %s", iteration, exc
                    )
                    final_content = f"[error] LLM call failed: {exc}"
                    break

                if response_tool_calls:
                    # Append assistant turn with tool call requests
                    messages.append(Message(
                        "assistant",
                        accumulated or "",
                        tool_calls=response_tool_calls,
                    ))
                    # Execute tools — per-tool isolation + Deterministic Middle
                    results = await self._execute_tool_calls(
                        response_tool_calls, messages, session_key
                    )
                    # Expand ephemeral set with schemas returned by search_tools
                    if "search_tools" in results and active_schemas is not None:
                        try:
                            found: list[dict[str, Any]] = json.loads(results["search_tools"])
                            if isinstance(found, list):
                                existing = {
                                    s["function"]["name"]
                                    for s in active_schemas
                                    if "function" in s
                                }
                                for schema in found:
                                    fn_name = schema.get("function", {}).get("name")
                                    if fn_name and fn_name not in existing:
                                        active_schemas.append(schema)
                                        existing.add(fn_name)
                        except (ValueError, TypeError, KeyError):
                            pass
                    iteration += 1
                    continue  # next iteration with tool results in context

                # No tool calls — LLM produced its final answer
                final_content = accumulated
                break

        finally:
            # Clean up abort event regardless of how the loop exited (Rec 7)
            self._abort_events.pop(session_key, None)

        logger.info(
            "Session %s → %s:%s (%d chars)",
            session_key, msg.platform, msg.channel_id, len(final_content),
        )

        return final_content
