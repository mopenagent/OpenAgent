"""Shared MCP-lite Unix socket client for platform service adapters."""

from __future__ import annotations

import asyncio
import json
import logging
import time
import uuid
from collections.abc import Callable
from pathlib import Path
from typing import Any

from openagent.observability import log_event
from openagent.observability.context import set_request_id
from openagent.observability.logging import get_logger
from openagent.observability.metrics import MCP_EVENTS_TOTAL, MCP_REQUEST_SECONDS, MCP_REQUEST_TOTAL
from openagent.services import protocol as proto

logger = get_logger(__name__)


class McpLiteClient:
    """Async MCP-lite client with request/response correlation and event hook."""

    def __init__(self, *, socket_path: str | Path):
        self._socket_path = str(socket_path)
        self._service_name = Path(self._socket_path).stem
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None
        self._read_task: asyncio.Task[None] | None = None
        self._pending: dict[str, asyncio.Future[proto.FrameModel]] = {}
        self._write_lock = asyncio.Lock()
        self._running = False
        self._event_handlers: list[Callable[[proto.EventFrame], None]] = []

    @property
    def socket_path(self) -> str:
        return self._socket_path

    @property
    def running(self) -> bool:
        return self._running

    async def start(self) -> None:
        self._reader, self._writer = await asyncio.open_unix_connection(self._socket_path)
        self._running = True
        log_event(
            logger,
            logging.INFO,
            "mcp client connected",
            component="mcp.client",
            operation="start",
            service=self._service_name,
            socket_path=self._socket_path,
        )
        self._read_task = asyncio.create_task(self._read_loop())
        await self.request({"type": "tools.list"})

    async def stop(self) -> None:
        self._running = False
        if self._writer:
            self._writer.close()
            await self._writer.wait_closed()
        self._writer = None
        self._reader = None

        if self._read_task:
            self._read_task.cancel()
            try:
                await self._read_task
            except asyncio.CancelledError:
                pass
        self._read_task = None
        log_event(
            logger,
            logging.INFO,
            "mcp client stopped",
            component="mcp.client",
            operation="stop",
            service=self._service_name,
        )

        for future in self._pending.values():
            if not future.done():
                future.cancel()
        self._pending.clear()

    async def request(
        self,
        payload: dict[str, Any],
        *,
        timeout_s: float = 5.0,
    ) -> proto.FrameModel:
        writer = self._writer
        if not writer:
            raise RuntimeError("MCP-lite client is not connected.")

        frame_id = str(uuid.uuid4())
        set_request_id(frame_id)
        frame_type = str(payload.get("type", "unknown"))
        tool_name = str(payload.get("tool", "")) if payload.get("type") == "tool.call" else ""
        start = time.perf_counter()
        full_payload = {"id": frame_id, **payload}
        line = json.dumps(full_payload, separators=(",", ":")) + "\n"

        async with self._write_lock:
            loop = asyncio.get_running_loop()
            future: asyncio.Future[proto.FrameModel] = loop.create_future()
            self._pending[frame_id] = future
            writer.write(line.encode("utf-8"))
            await writer.drain()

        try:
            frame = await asyncio.wait_for(future, timeout=timeout_s)
        except TimeoutError:
            elapsed = max(time.perf_counter() - start, timeout_s)
            MCP_REQUEST_TOTAL.labels(
                service=self._service_name,
                type=frame_type,
                tool=tool_name,
                status="timeout",
            ).inc()
            MCP_REQUEST_SECONDS.labels(
                service=self._service_name,
                type=frame_type,
                tool=tool_name,
                status="timeout",
            ).observe(elapsed)
            log_event(
                logger,
                logging.ERROR,
                "mcp request timed out",
                component="mcp.client",
                operation="request",
                service=self._service_name,
                request_id=frame_id,
                frame_type=frame_type,
                tool=tool_name,
                timeout_s=timeout_s,
            )
            raise
        finally:
            self._pending.pop(frame_id, None)
            set_request_id(None)

        if isinstance(frame, proto.ProtocolErrorFrame):
            elapsed = time.perf_counter() - start
            MCP_REQUEST_TOTAL.labels(
                service=self._service_name,
                type=frame_type,
                tool=tool_name,
                status="error",
            ).inc()
            MCP_REQUEST_SECONDS.labels(
                service=self._service_name,
                type=frame_type,
                tool=tool_name,
                status="error",
            ).observe(elapsed)
            log_event(
                logger,
                logging.ERROR,
                "mcp request failed",
                component="mcp.client",
                operation="request",
                service=self._service_name,
                request_id=frame_id,
                frame_type=frame_type,
                tool=tool_name,
                code=frame.code,
                error=frame.message,
            )
            raise RuntimeError(f"{frame.code}: {frame.message}")
        elapsed = time.perf_counter() - start
        MCP_REQUEST_TOTAL.labels(
            service=self._service_name,
            type=frame_type,
            tool=tool_name,
            status="ok",
        ).inc()
        MCP_REQUEST_SECONDS.labels(
            service=self._service_name,
            type=frame_type,
            tool=tool_name,
            status="ok",
        ).observe(elapsed)
        return frame

    async def _read_loop(self) -> None:
        reader = self._reader
        if not reader:
            return
        while True:
            line = await reader.readline()
            if not line:
                self._running = False
                log_event(
                    logger,
                    logging.WARNING,
                    "mcp socket closed by peer",
                    component="mcp.client",
                    operation="read_loop",
                    service=self._service_name,
                )
                break
            try:
                frame = proto.parse_frame(line)
            except Exception as exc:
                log_event(
                    logger,
                    logging.ERROR,
                    "failed to parse mcp frame",
                    component="mcp.client",
                    operation="parse",
                    service=self._service_name,
                    error=str(exc),
                )
                continue
            if isinstance(frame, proto.EventFrame):
                MCP_EVENTS_TOTAL.labels(service=self._service_name, event=frame.event).inc()
                self.on_event(frame)
                continue
            request_id = getattr(frame, "id", None)
            if not request_id:
                continue
            set_request_id(request_id)
            future = self._pending.get(request_id)
            if future and not future.done():
                future.set_result(frame)
            set_request_id(None)

    def add_event_handler(self, handler: Callable[[proto.EventFrame], None]) -> None:
        """Register a callback invoked for every EventFrame received.

        Use this instead of subclassing when you need to attach behaviour to
        an existing client instance (e.g. from ServiceManager).  Subclasses
        that override ``on_event`` without calling ``super()`` will bypass
        registered handlers.
        """
        self._event_handlers.append(handler)

    def on_event(self, frame: proto.EventFrame) -> None:
        """Called for each EventFrame. Override in subclasses OR use add_event_handler()."""
        for handler in self._event_handlers:
            handler(frame)
