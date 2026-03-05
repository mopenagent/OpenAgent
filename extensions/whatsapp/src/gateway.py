"""Gateway service loop for WhatsApp extension."""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from threading import Lock, Thread
from typing import Any

from handlers import WhatsAppHandlers
from heartbeat import HeartbeatTracker
from session import GatewayClient, SessionManager


@dataclass(slots=True)
class GatewayConfig:
    reconnect_base_seconds: float = 1.0
    reconnect_max_seconds: float = 30.0
    max_reconnect_attempts: int = 0  # 0 means unlimited


class WhatsAppGateway:
    def __init__(
        self,
        *,
        session: SessionManager,
        handlers: WhatsAppHandlers,
        heartbeat: HeartbeatTracker,
        config: GatewayConfig | None = None,
    ):
        self._session = session
        self._handlers = handlers
        self._heartbeat = heartbeat
        self._config = config or GatewayConfig()
        self._stop = False
        self._thread: Thread | None = None
        self._loop: asyncio.AbstractEventLoop | None = None
        self._run_task: asyncio.Task[None] | None = None
        self._event_task: asyncio.Task[None] | None = None
        self._event_queue: asyncio.Queue[Any] | None = None
        self._client: GatewayClient | None = None
        self._latest_qr: str | None = None
        self._lock = Lock()

    @property
    def latest_qr(self) -> str | None:
        with self._lock:
            return self._latest_qr

    def get_client(self) -> GatewayClient | None:
        return self._client

    def start(self) -> None:
        if self._thread and self._thread.is_alive():
            return
        self._stop = False
        self._thread = Thread(target=self._run_loop, name="openagent-whatsapp-gateway", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop = True
        loop = self._loop
        if loop and loop.is_running():
            asyncio.run_coroutine_threadsafe(self._stop_async(), loop)
        if self._thread and self._thread.is_alive():
            self._thread.join(timeout=3)

    def _run_loop(self) -> None:
        self._loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self._loop)
        self._event_queue = asyncio.Queue()
        self._run_task = self._loop.create_task(self._run())
        self._event_task = self._loop.create_task(self._process_events())
        try:
            self._loop.run_until_complete(asyncio.gather(self._run_task, self._event_task))
        finally:
            pending = asyncio.all_tasks(self._loop)
            for task in pending:
                task.cancel()
            if pending:
                self._loop.run_until_complete(asyncio.gather(*pending, return_exceptions=True))
            self._loop.close()
            self._loop = None

    async def _run(self) -> None:
        attempts = 0
        self._heartbeat.mark_running(True)
        while not self._stop:
            try:
                linked = await asyncio.to_thread(self._session.is_linked)
                auth_age_ms = await asyncio.to_thread(self._session.auth_age_ms)
                self_id = await asyncio.to_thread(self._session.read_self_id)
                self._heartbeat.set_linked(
                    linked=linked,
                    auth_age_ms=auth_age_ms,
                    self_id=self_id,
                )
                self._client = await asyncio.to_thread(
                    self._session.create_client,
                    on_qr=self._on_qr,
                    on_event=self._on_event,
                )
                await asyncio.to_thread(self._client.connect)
                attempts = 0
                self._heartbeat.set_reconnect_attempts(0)
                connected_self_id = await asyncio.to_thread(self._session.read_self_id)
                self._heartbeat.mark_connected(self_id=connected_self_id)

                while not self._stop and await asyncio.to_thread(self._client.is_connected):
                    await asyncio.sleep(0.25)

                if self._stop:
                    break

                self._heartbeat.mark_disconnected(reason="connection-lost")
                raise RuntimeError("whatsapp client disconnected")
            except Exception as exc:
                if self._stop:
                    break
                attempts += 1
                self._heartbeat.mark_error(exc)
                self._heartbeat.mark_disconnected(reason=str(exc))
                self._heartbeat.set_reconnect_attempts(attempts)
                if self._config.max_reconnect_attempts > 0 and attempts >= self._config.max_reconnect_attempts:
                    break
                delay = min(
                    self._config.reconnect_max_seconds,
                    self._config.reconnect_base_seconds * (2 ** max(0, attempts - 1)),
                )
                await asyncio.sleep(delay)
            finally:
                if self._client:
                    try:
                        await asyncio.to_thread(self._client.disconnect)
                    except Exception:
                        pass

        self._heartbeat.mark_running(False)
        self._heartbeat.mark_disconnected(reason="stopped")
        self._stop = True
        if self._event_queue:
            self._event_queue.put_nowait(None)

    def _on_qr(self, qr_text: str) -> None:
        with self._lock:
            self._latest_qr = qr_text
        self._heartbeat.mark_event()

    def _on_event(self, event: Any) -> None:
        loop = self._loop
        queue = self._event_queue
        if not loop or not queue:
            return
        loop.call_soon_threadsafe(queue.put_nowait, event)

    async def _process_events(self) -> None:
        queue = self._event_queue
        if not queue:
            return
        while True:
            event = await queue.get()
            if self._stop and event is None:
                break
            try:
                await self._handlers.handle_event(event)
            except Exception as exc:
                self._heartbeat.mark_error(exc)

    async def _stop_async(self) -> None:
        self._stop = True
        if self._event_queue:
            self._event_queue.put_nowait(None)
        client = self._client
        if client:
            try:
                await asyncio.to_thread(client.disconnect)
            except Exception:
                pass

    async def send_text(self, channel_id: str, text: str) -> Any:
        client = self._client
        if not client:
            raise RuntimeError("WhatsApp gateway is not connected.")
        return await asyncio.to_thread(client.send_message, channel_id, {"type": "text", "text": text})
