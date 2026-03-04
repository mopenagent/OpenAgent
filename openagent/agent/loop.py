"""AgentLoop — custom ReAct loop. No framework dependency.

Architecture
------------
The loop reads from the MessageBus per-session queue, runs one full
ReAct iteration (LLM → tool calls → LLM → ... → final reply), saves turns
to the SessionManager, then dispatches the reply via the bus.

One coroutine per active session runs concurrently; they share the provider
and tool registry but each owns its own history slice.

Iteration limit
---------------
``MAX_ITERATIONS = 40`` prevents infinite loops when a model keeps calling
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
cross-channel identity (canonical_id → "user:alice") and per-chat fallback.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from openagent.bus.bus import MessageBus
from openagent.bus.events import InboundMessage, OutboundMessage
from openagent.providers.base import LLMResponse, Message, Provider
from openagent.session.manager import SessionManager
from openagent.agent.tools import ToolRegistry

logger = logging.getLogger(__name__)

MAX_ITERATIONS = 40
MAX_TOOL_OUTPUT = 500   # chars; truncate beyond this
_SYSTEM_PROMPT = (
    "You are a helpful assistant. "
    "Use tools only when necessary. "
    "Be concise."
)


class AgentLoop:
    """Orchestrates InboundMessage → LLM → tools → OutboundMessage.

    Parameters
    ----------
    bus:        MessageBus — publish/subscribe point for all channels.
    provider:   LLM provider implementing ``chat(messages, tools)``.
    sessions:   SessionManager — history persistence.
    tools:      ToolRegistry — maps tool names to Go/Rust services.
    system_prompt:
        Override the default system prompt.
    """

    def __init__(
        self,
        bus: MessageBus,
        provider: Provider,
        sessions: SessionManager,
        tools: ToolRegistry,
        *,
        system_prompt: str = _SYSTEM_PROMPT,
    ) -> None:
        self._bus = bus
        self._provider = provider
        self._sessions = sessions
        self._tools = tools
        self._system_prompt = system_prompt
        self._tasks: dict[str, asyncio.Task[None]] = {}
        self._running = False

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
        q = self._bus.session_queue(session_key)
        while self._running:
            msg = await q.get()
            if msg is None:  # shutdown sentinel
                break
            try:
                await self._process(msg)
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.exception(
                    "Unhandled error processing message from session %s", session_key
                )

    # ------------------------------------------------------------------
    # Core ReAct loop
    # ------------------------------------------------------------------

    async def _process(self, msg: InboundMessage) -> None:
        """Run one full ReAct turn for an inbound message."""
        session_key = msg.session_key

        # Save user turn
        await self._sessions.append(session_key, "user", msg.content)

        # Build message list: system + history (excludes the turn we just added
        # so we don't double-count; get_history includes it)
        history = await self._sessions.get_history(session_key)
        messages: list[Message] = [
            Message("system", self._system_prompt),
            *self._sessions.to_messages(history),
        ]

        tool_schemas = self._tools.schemas() if self._tools.has_tools() else None
        final_content = ""

        for iteration in range(MAX_ITERATIONS):
            try:
                response: LLMResponse = await self._provider.chat(
                    messages, tools=tool_schemas
                )
            except Exception as exc:
                logger.error("LLM call failed on iteration %d: %s", iteration, exc)
                final_content = f"[error] LLM call failed: {exc}"
                break

            if response.content:
                final_content = response.content

            if not response.has_tool_calls:
                # Model gave a final answer — done
                break

            # Append assistant turn with tool_calls to context
            messages.append(Message("assistant", response.content or ""))

            # Execute each tool call and inject results
            for tc in response.tool_calls:
                logger.debug("Tool call: %s(%s)", tc.name, tc.arguments)
                raw_result = await self._tools.call(tc.name, tc.arguments)

                # Truncate tool output
                if len(raw_result) > MAX_TOOL_OUTPUT:
                    logger.debug(
                        "Truncating tool output for %s: %d → %d chars",
                        tc.name, len(raw_result), MAX_TOOL_OUTPUT,
                    )
                    raw_result = raw_result[:MAX_TOOL_OUTPUT] + "…[truncated]"

                # Save tool result to history
                await self._sessions.append(
                    session_key, "tool", raw_result,
                    tool_call_id=tc.id,
                    tool_name=tc.name,
                )
                messages.append(Message(
                    "tool", raw_result,
                    tool_call_id=tc.id,
                    tool_name=tc.name,
                ))

        else:
            # Hit iteration limit
            logger.warning(
                "Session %s hit MAX_ITERATIONS=%d; returning partial reply",
                session_key, MAX_ITERATIONS,
            )
            if not final_content:
                final_content = "[max iterations reached — partial response]"

        # Save assistant final turn
        if final_content:
            await self._sessions.append(session_key, "assistant", final_content)

        # Dispatch reply to the originating channel.
        # Copy inbound metadata so channel adapters (e.g. Telegram) can read
        # access_hash and other peer identifiers needed to send the reply.
        reply = OutboundMessage(
            channel=msg.channel,
            chat_id=msg.chat_id,
            content=final_content,
            session_key=session_key,
            metadata=dict(msg.metadata),
        )
        await self._bus.dispatch(reply)
        logger.info(
            "Session %s → %s:%s (%d chars)",
            session_key, msg.channel, msg.chat_id, len(final_content),
        )
