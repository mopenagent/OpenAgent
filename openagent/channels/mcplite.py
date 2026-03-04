"""Shared MCP-lite Unix socket client for channel service adapters."""

from __future__ import annotations

import asyncio
import json
import uuid
from pathlib import Path
from typing import Any

from openagent.services import protocol as proto


class McpLiteClient:
    """Async MCP-lite client with request/response correlation and event hook."""

    def __init__(self, *, socket_path: str | Path):
        self._socket_path = str(socket_path)
        self._reader: asyncio.StreamReader | None = None
        self._writer: asyncio.StreamWriter | None = None
        self._read_task: asyncio.Task[None] | None = None
        self._pending: dict[str, asyncio.Future[proto.FrameModel]] = {}
        self._write_lock = asyncio.Lock()
        self._running = False

    @property
    def socket_path(self) -> str:
        return self._socket_path

    @property
    def running(self) -> bool:
        return self._running

    async def start(self) -> None:
        self._reader, self._writer = await asyncio.open_unix_connection(self._socket_path)
        self._running = True
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
        finally:
            self._pending.pop(frame_id, None)

        if isinstance(frame, proto.ProtocolErrorFrame):
            raise RuntimeError(f"{frame.code}: {frame.message}")
        return frame

    async def _read_loop(self) -> None:
        reader = self._reader
        if not reader:
            return
        while True:
            line = await reader.readline()
            if not line:
                self._running = False
                break
            frame = proto.parse_frame(line)
            if isinstance(frame, proto.EventFrame):
                self.on_event(frame)
                continue
            request_id = getattr(frame, "id", None)
            if not request_id:
                continue
            future = self._pending.get(request_id)
            if future and not future.done():
                future.set_result(frame)

    def on_event(self, frame: proto.EventFrame) -> None:
        """Override in subclasses to handle event frames."""
