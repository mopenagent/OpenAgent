"""OpenAgent heartbeat service for periodic service/options polling."""

from __future__ import annotations

import asyncio
import json
import os
from dataclasses import dataclass, field
from pathlib import Path
import time
from typing import Any
from uuid import uuid4

from openagent.observability import get_logger, log_event
from openagent.providers import load_provider_config
from openagent.services import protocol as proto

logger = get_logger(__name__)


@dataclass(slots=True)
class ServiceHeartbeat:
    name: str
    socket: str
    version: str
    tools: int
    events: int
    status: str
    latency_ms: float | None = None
    error: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "socket": self.socket,
            "version": self.version,
            "tools": self.tools,
            "events": self.events,
            "status": self.status,
            "latency_ms": self.latency_ms,
            "error": self.error,
        }


@dataclass(slots=True)
class HeartbeatSnapshot:
    tick: int
    at: float
    duration_ms: float
    services_total: int
    services_online: int
    services_offline: int
    provider: dict[str, Any]
    services: list[ServiceHeartbeat] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "tick": self.tick,
            "at": self.at,
            "duration_ms": self.duration_ms,
            "services_total": self.services_total,
            "services_online": self.services_online,
            "services_offline": self.services_offline,
            "provider": dict(self.provider),
            "services": [svc.to_dict() for svc in self.services],
        }


class HeartbeatService:
    """Periodic poller that reports OpenAgent service + options state."""

    def __init__(
        self,
        *,
        root: Path,
        interval_s: float = 30.0,
        enabled: bool = True,
        provider_config_path: Path | None = None,
    ):
        self.root = root
        self.interval_s = max(1.0, float(interval_s))
        self.enabled = enabled
        self.provider_config_path = provider_config_path
        self._running = False
        self._task: asyncio.Task[None] | None = None
        self._tick = 0
        self._last_snapshot: HeartbeatSnapshot | None = None

    @property
    def last_snapshot(self) -> HeartbeatSnapshot | None:
        return self._last_snapshot

    async def start(self) -> None:
        if not self.enabled:
            log_event(
                logger,
                20,
                "heartbeat disabled",
                component="heartbeat",
                operation="start",
            )
            return
        if self._running:
            return

        self._running = True
        await self.tick()
        self._task = asyncio.create_task(self._run_loop())
        log_event(
            logger,
            20,
            "heartbeat started",
            component="heartbeat",
            operation="start",
            interval_s=self.interval_s,
        )

    async def stop(self) -> None:
        self._running = False
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass
            self._task = None
        log_event(
            logger,
            20,
            "heartbeat stopped",
            component="heartbeat",
            operation="stop",
        )

    async def _run_loop(self) -> None:
        while self._running:
            try:
                await asyncio.sleep(self.interval_s)
                if self._running:
                    await self.tick()
            except asyncio.CancelledError:
                break
            except Exception as exc:
                log_event(
                    logger,
                    40,
                    "heartbeat loop error",
                    component="heartbeat",
                    operation="run_loop",
                    error=str(exc),
                )

    async def tick(self) -> HeartbeatSnapshot:
        start = time.perf_counter()
        self._tick += 1
        manifests = self._discover_service_manifests()
        service_states = await asyncio.gather(
            *(self._poll_service(manifest) for manifest in manifests),
            return_exceptions=False,
        )

        online = sum(1 for item in service_states if item.status == "online")
        offline = len(service_states) - online

        provider_cfg = load_provider_config(self.provider_config_path)
        provider = {
            "kind": provider_cfg.kind,
            "base_url": provider_cfg.base_url,
            "model": provider_cfg.model,
            "timeout": float(provider_cfg.timeout),
            "max_tokens": int(provider_cfg.max_tokens),
        }

        duration_ms = (time.perf_counter() - start) * 1000.0
        snapshot = HeartbeatSnapshot(
            tick=self._tick,
            at=time.time(),
            duration_ms=duration_ms,
            services_total=len(service_states),
            services_online=online,
            services_offline=offline,
            provider=provider,
            services=service_states,
        )
        self._last_snapshot = snapshot

        log_event(
            logger,
            20,
            "heartbeat tick",
            component="heartbeat",
            operation="tick",
            tick=snapshot.tick,
            services_total=snapshot.services_total,
            services_online=snapshot.services_online,
            services_offline=snapshot.services_offline,
            service_state={svc.name: svc.status for svc in snapshot.services},
            provider_kind=provider["kind"],
            provider_model=provider["model"],
            duration_ms=round(snapshot.duration_ms, 3),
        )
        return snapshot

    def _discover_service_manifests(self) -> list[dict[str, Any]]:
        services_dir = self.root / "services"
        manifests: list[dict[str, Any]] = []
        if not services_dir.exists():
            return manifests

        for manifest_path in sorted(services_dir.glob("*/service.json")):
            try:
                data = json.loads(manifest_path.read_text(encoding="utf-8"))
            except Exception:
                continue
            if not isinstance(data, dict):
                continue
            data["_manifest_path"] = str(manifest_path)
            manifests.append(data)
        return manifests

    async def _poll_service(self, manifest: dict[str, Any]) -> ServiceHeartbeat:
        name = str(manifest.get("name") or Path(str(manifest.get("_manifest_path", ""))).parent.name)
        socket_raw = str(manifest.get("socket") or f"data/sockets/{name}.sock")
        socket_path = Path(socket_raw)
        if not socket_path.is_absolute():
            socket_path = self.root / socket_path

        timeout_ms = 1000
        health = manifest.get("health")
        if isinstance(health, dict):
            timeout_val = health.get("timeout_ms")
            if isinstance(timeout_val, (int, float)) and timeout_val > 0:
                timeout_ms = int(timeout_val)

        start = time.perf_counter()
        status = "offline"
        error: str | None = None

        try:
            reader, writer = await asyncio.wait_for(
                asyncio.open_unix_connection(str(socket_path)),
                timeout=timeout_ms / 1000.0,
            )
            try:
                frame_id = str(uuid4())
                ping = {"id": frame_id, "type": "ping"}
                writer.write((json.dumps(ping, separators=(",", ":")) + "\n").encode("utf-8"))
                await writer.drain()
                line = await asyncio.wait_for(reader.readline(), timeout=timeout_ms / 1000.0)
                parsed = proto.parse_frame(line)
                if isinstance(parsed, proto.ProtocolPong):
                    status = "online"
                else:
                    status = "degraded"
                    error = f"unexpected frame: {type(parsed).__name__}"
            finally:
                writer.close()
                await writer.wait_closed()
        except Exception as exc:
            status = "offline"
            error = str(exc)

        latency_ms = (time.perf_counter() - start) * 1000.0
        tools = manifest.get("tools") if isinstance(manifest.get("tools"), list) else []
        events = manifest.get("events") if isinstance(manifest.get("events"), list) else []

        return ServiceHeartbeat(
            name=name,
            socket=str(socket_path),
            version=str(manifest.get("version", "?")),
            tools=len(tools),
            events=len(events),
            status=status,
            latency_ms=round(latency_ms, 3),
            error=error,
        )



def heartbeat_enabled_from_env() -> bool:
    return os.getenv("OPENAGENT_HEARTBEAT_ENABLED", "1").strip().lower() not in {"0", "false", "no"}



def heartbeat_interval_from_env(default: float = 30.0) -> float:
    raw = os.getenv("OPENAGENT_HEARTBEAT_INTERVAL_S")
    if not raw:
        return default
    try:
        value = float(raw)
    except ValueError:
        return default
    return max(1.0, value)
