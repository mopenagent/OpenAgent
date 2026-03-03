"""Runtime heartbeat and status snapshot tracking."""

from __future__ import annotations

from dataclasses import asdict, dataclass
from threading import Lock
from time import time
from typing import Any


@dataclass(slots=True)
class DisconnectInfo:
    at: float
    reason: str | None = None
    code: int | None = None


@dataclass(slots=True)
class HeartbeatSnapshot:
    running: bool = False
    connected: bool = False
    reconnect_attempts: int = 0
    last_connected_at: float | None = None
    last_disconnect: DisconnectInfo | None = None
    last_message_at: float | None = None
    last_event_at: float | None = None
    last_error: str | None = None
    linked: bool = False
    self_id: str | None = None
    auth_age_ms: int | None = None

    def to_dict(self) -> dict[str, Any]:
        data = asdict(self)
        if self.last_disconnect:
            data["last_disconnect"] = asdict(self.last_disconnect)
        return data


class HeartbeatTracker:
    def __init__(self) -> None:
        self._state = HeartbeatSnapshot()
        self._lock = Lock()

    def mark_running(self, running: bool = True) -> None:
        with self._lock:
            self._state.running = running
            self._state.last_event_at = time()

    def mark_connected(self, *, self_id: str | None = None) -> None:
        with self._lock:
            self._state.connected = True
            self._state.last_connected_at = time()
            self._state.last_event_at = self._state.last_connected_at
            self._state.last_error = None
            if self_id:
                self._state.self_id = self_id

    def mark_disconnected(self, *, reason: str | None = None, code: int | None = None) -> None:
        with self._lock:
            now = time()
            self._state.connected = False
            self._state.last_event_at = now
            self._state.last_disconnect = DisconnectInfo(at=now, reason=reason, code=code)

    def mark_message(self) -> None:
        with self._lock:
            now = time()
            self._state.last_message_at = now
            self._state.last_event_at = now

    def mark_event(self) -> None:
        with self._lock:
            self._state.last_event_at = time()

    def mark_error(self, error: Exception | str) -> None:
        with self._lock:
            self._state.last_error = str(error)
            self._state.last_event_at = time()

    def set_reconnect_attempts(self, attempts: int) -> None:
        with self._lock:
            self._state.reconnect_attempts = max(0, attempts)
            self._state.last_event_at = time()

    def set_linked(
        self,
        *,
        linked: bool,
        auth_age_ms: int | None = None,
        self_id: str | None = None,
    ) -> None:
        with self._lock:
            self._state.linked = linked
            self._state.auth_age_ms = auth_age_ms
            if self_id:
                self._state.self_id = self_id
            self._state.last_event_at = time()

    def snapshot(self) -> HeartbeatSnapshot:
        with self._lock:
            current = self._state
            return HeartbeatSnapshot(
                running=current.running,
                connected=current.connected,
                reconnect_attempts=current.reconnect_attempts,
                last_connected_at=current.last_connected_at,
                last_disconnect=(
                    DisconnectInfo(
                        at=current.last_disconnect.at,
                        reason=current.last_disconnect.reason,
                        code=current.last_disconnect.code,
                    )
                    if current.last_disconnect
                    else None
                ),
                last_message_at=current.last_message_at,
                last_event_at=current.last_event_at,
                last_error=current.last_error,
                linked=current.linked,
                self_id=current.self_id,
                auth_age_ms=current.auth_age_ms,
            )
