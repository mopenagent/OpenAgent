"""ServiceManager — spawn, watch, and reconnect Go service binaries."""

from __future__ import annotations

import asyncio
import json
import logging
import os
import platform
import sys
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any

import psutil

from openagent.platforms.mcplite import McpLiteClient
from openagent.observability import log_event
from openagent.observability.logging import get_logger

logger = get_logger(__name__)

# Seconds to wait for socket file to appear after process is spawned
_SOCKET_WAIT_S = 10.0
_SOCKET_POLL_S = 0.05


# ---------------------------------------------------------------------------
# Manifest model
# ---------------------------------------------------------------------------


@dataclass(slots=True)
class HealthConfig:
    interval_ms: int = 5000
    timeout_ms: int = 1000
    restart_backoff_ms: list[int] = field(
        default_factory=lambda: [1000, 2000, 5000, 10000, 30000]
    )


@dataclass(slots=True)
class ServiceManifest:
    name: str
    description: str
    version: str
    binary: dict[str, str]
    socket: str
    health: HealthConfig
    tools: list[dict[str, Any]]
    events: list[dict[str, Any]]
    manifest_path: Path
    runtime: str  # "go" | "rust"

    @classmethod
    def from_dict(cls, data: dict[str, Any], manifest_path: Path) -> ServiceManifest:
        health_raw = data.get("health") or {}
        backoff_raw = health_raw.get("restart_backoff_ms") or [1000, 2000, 5000, 10000, 30000]
        health = HealthConfig(
            interval_ms=int(health_raw.get("interval_ms", 5000)),
            timeout_ms=int(health_raw.get("timeout_ms", 1000)),
            restart_backoff_ms=[int(x) for x in backoff_raw],
        )
        name = str(data.get("name") or manifest_path.parent.name)
        return cls(
            name=name,
            description=str(data.get("description", "")),
            version=str(data.get("version", "0.0.0")),
            binary=dict(data.get("binary") or {}),
            socket=str(data.get("socket") or f"data/sockets/{name}.sock"),
            health=health,
            tools=list(data.get("tools") or []),
            events=list(data.get("events") or []),
            manifest_path=manifest_path,
            runtime=str(data.get("runtime", "go")),
        )


# ---------------------------------------------------------------------------
# Managed service runtime state
# ---------------------------------------------------------------------------


class ServiceStatus(str, Enum):
    STARTING = "starting"
    RUNNING = "running"
    RESTARTING = "restarting"
    STOPPED = "stopped"
    NO_BINARY = "no_binary"  # binary not present for current platform


class ManagedService:
    """Runtime state for one managed Go service."""

    __slots__ = (
        "manifest",
        "name",
        "status",
        "restart_count",
        "last_error",
        "client",
        "_process",
    )

    def __init__(self, manifest: ServiceManifest) -> None:
        self.manifest = manifest
        self.name = manifest.name
        self.status = ServiceStatus.STOPPED
        self.restart_count = 0
        self.last_error: str | None = None
        self.client: McpLiteClient | None = None
        self._process: asyncio.subprocess.Process | None = None

    def to_dict(self) -> dict[str, Any]:
        out: dict[str, Any] = {
            "name": self.name,
            "status": self.status.value,
            "restart_count": self.restart_count,
            "last_error": self.last_error,
            "version": self.manifest.version,
            "description": self.manifest.description,
            "tools": self.manifest.tools,
            "events": self.manifest.events,
            "socket": self.manifest.socket,
            "runtime": self.manifest.runtime,
        }
        # Process memory (RSS) in MB when running
        if self._process and self._process.returncode is None and self._process.pid:
            try:
                p = psutil.Process(self._process.pid)
                out["memory_mb"] = round(p.memory_info().rss / (1024 * 1024), 1)
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                out["memory_mb"] = None
        else:
            out["memory_mb"] = None
        return out


# ---------------------------------------------------------------------------
# Platform detection
# ---------------------------------------------------------------------------


def _current_platform() -> str:
    """Return the service.json binary key for the current OS/arch."""
    if sys.platform == "darwin":
        return "darwin/arm64"
    machine = platform.machine().lower()
    if machine in ("aarch64", "arm64"):
        return "linux/arm64"
    return "linux/amd64"


# ---------------------------------------------------------------------------
# ServiceManager
# ---------------------------------------------------------------------------


class ServiceManager:
    """Spawn, watch, and reconnect Go service binaries via MCP-lite sockets.

    Each service listed in ``services/*/service.json`` is started as a
    subprocess. A persistent ``McpLiteClient`` is maintained per service.
    On crash or exit the service is restarted with exponential back-off as
    defined in the manifest's ``health.restart_backoff_ms`` list.

    Usage::

        mgr = ServiceManager(root=ROOT)
        await mgr.start()
        ...
        client = mgr.get_client("discord")
        ...
        await mgr.stop()
    """

    def __init__(
        self,
        *,
        root: Path,
        env_extras: dict[str, dict[str, str]] | None = None,
    ) -> None:
        self._root = root
        self._platform = _current_platform()
        self._services: dict[str, ManagedService] = {}
        self._watchdog_tasks: dict[str, asyncio.Task[None]] = {}
        self._running = False
        # Per-service extra env vars injected on each launch (e.g. platform tokens).
        # Keys are service names; values are dicts merged into the subprocess env.
        self._env_extras: dict[str, dict[str, str]] = env_extras or {}
        # Optional async callback fired each time a service becomes ready.
        # Signature: async fn(service_name: str) -> None
        self._service_ready_cb: Callable[[str], Awaitable[None]] | None = None

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    async def start(self) -> None:
        """Discover manifests and start a watchdog task per service."""
        self._running = True
        manifests = self._discover_manifests()
        for manifest in manifests:
            svc = ManagedService(manifest)
            self._services[manifest.name] = svc
            task = asyncio.create_task(
                self._watchdog(svc), name=f"svcmgr.watchdog.{svc.name}"
            )
            self._watchdog_tasks[svc.name] = task
        log_event(
            logger,
            logging.INFO,
            "service manager started",
            component="service_manager",
            operation="start",
            services=list(self._services),
            platform=self._platform,
        )

    async def stop(self) -> None:
        """Cancel all watchdogs and gracefully terminate services."""
        self._running = False
        for task in self._watchdog_tasks.values():
            task.cancel()
        if self._watchdog_tasks:
            await asyncio.gather(*self._watchdog_tasks.values(), return_exceptions=True)
        self._watchdog_tasks.clear()
        for svc in self._services.values():
            await self._teardown(svc)
        log_event(
            logger,
            logging.INFO,
            "service manager stopped",
            component="service_manager",
            operation="stop",
        )

    def on_service_ready(self, cb: Callable[[str], Awaitable[None]]) -> None:
        """Register an async callback invoked each time a service becomes ready.

        Called after every successful launch, including restarts.
        Signature: ``async fn(service_name: str) -> None``.
        Used by ``ToolRegistry.register_service`` to load tools per-service.
        """
        self._service_ready_cb = cb

    def get_client(self, name: str) -> McpLiteClient | None:
        """Return the running MCP-lite client for *name*, or None."""
        svc = self._services.get(name)
        if svc and svc.client and svc.client.running:
            return svc.client
        return None

    def list_services(self) -> list[ManagedService]:
        return list(self._services.values())

    async def stop_service(self, name: str) -> bool:
        """Permanently stop a service without restarting (for Settings enable/disable)."""
        svc = self._services.get(name)
        if not svc:
            return False
        task = self._watchdog_tasks.pop(name, None)
        if task:
            task.cancel()
            try:
                await asyncio.wait_for(asyncio.shield(task), timeout=5.0)
            except (asyncio.CancelledError, asyncio.TimeoutError):
                pass
        await self._teardown(svc)
        svc.status = ServiceStatus.STOPPED
        log_event(
            logger,
            logging.INFO,
            "service stopped by operator",
            component="service_manager",
            operation="stop_service",
            service=name,
        )
        return True

    async def reload(self, name: str) -> bool:
        """Gracefully terminate, reload manifest from disk, and respawn the service."""
        svc = self._services.get(name)
        if not svc:
            return False

        log_event(
            logger,
            logging.INFO,
            "reloading service",
            component="service_manager",
            operation="reload",
            service=name,
        )

        task = self._watchdog_tasks.get(name)
        if task:
            task.cancel()
            try:
                await task
            except asyncio.CancelledError:
                pass

        await self._teardown(svc)

        try:
            data = json.loads(svc.manifest.manifest_path.read_text(encoding="utf-8"))
            svc.manifest = ServiceManifest.from_dict(data, svc.manifest.manifest_path)
            svc.status = ServiceStatus.STOPPED
        except Exception as exc:
            log_event(
                logger,
                logging.ERROR,
                "failed to reload manifest",
                component="service_manager",
                operation="reload",
                service=name,
                error=str(exc),
            )
            return False

        new_task = asyncio.create_task(
            self._watchdog(svc), name=f"svcmgr.watchdog.{svc.name}"
        )
        self._watchdog_tasks[svc.name] = new_task
        return True

    # ------------------------------------------------------------------
    # Watchdog loop
    # ------------------------------------------------------------------

    async def _watchdog(self, svc: ManagedService) -> None:
        backoff = svc.manifest.health.restart_backoff_ms
        attempt = 0
        while self._running:
            health_task: asyncio.Task[None] | None = None
            try:
                await self._launch(svc)
                attempt = 0  # reset on successful start
                # Run health ping loop concurrently while the process is alive.
                # If it detects a timeout it calls process.terminate(), which
                # unblocks the process.wait() below.
                health_task = asyncio.create_task(
                    self._health_loop(svc), name=f"svcmgr.health.{svc.name}"
                )
                assert svc._process is not None
                await svc._process.wait()
                rc = svc._process.returncode
                if not self._running:
                    break
                log_event(
                    logger,
                    logging.WARNING,
                    "service process exited",
                    component="service_manager",
                    operation="watchdog",
                    service=svc.name,
                    returncode=rc,
                    restart_count=svc.restart_count,
                )
                svc.status = ServiceStatus.RESTARTING
                svc.last_error = f"process exited with code {rc}"
            except asyncio.CancelledError:
                break
            except Exception as exc:
                svc.last_error = str(exc)
                if svc.status == ServiceStatus.NO_BINARY:
                    # Binary absent — no point retrying until operator intervenes
                    break
                svc.status = ServiceStatus.RESTARTING
                log_event(
                    logger,
                    logging.ERROR,
                    "service launch failed",
                    component="service_manager",
                    operation="watchdog",
                    service=svc.name,
                    error=str(exc),
                    attempt=attempt,
                )
            finally:
                # Always cancel the health task when this launch attempt ends
                if health_task is not None and not health_task.done():
                    health_task.cancel()
                    try:
                        await health_task
                    except (asyncio.CancelledError, Exception):
                        pass

            # Disconnect client before sleeping
            if svc.client:
                try:
                    await svc.client.stop()
                except Exception:
                    pass
                svc.client = None

            delay_ms = backoff[min(attempt, len(backoff) - 1)]
            attempt += 1
            svc.restart_count += 1
            try:
                await asyncio.sleep(delay_ms / 1000.0)
            except asyncio.CancelledError:
                break

        await self._teardown(svc)
        svc.status = ServiceStatus.STOPPED

    async def _health_loop(self, svc: ManagedService) -> None:
        """Periodically ping the service; terminate the process on timeout."""
        interval_s = svc.manifest.health.interval_ms / 1000.0
        timeout_s = svc.manifest.health.timeout_ms / 1000.0
        while self._running:
            await asyncio.sleep(interval_s)
            if not svc.client or not svc.client.running:
                break
            try:
                await svc.client.request({"type": "ping"}, timeout_s=timeout_s)
            except TimeoutError:
                log_event(
                    logger,
                    logging.ERROR,
                    "health ping timed out — terminating service",
                    component="service_manager",
                    operation="health_loop",
                    service=svc.name,
                    timeout_s=timeout_s,
                )
                svc.last_error = f"health ping timed out after {timeout_s}s"
                if svc._process and svc._process.returncode is None:
                    svc._process.terminate()
                break
            except Exception:
                # Client disconnected — process likely already dead, watchdog handles it
                break

    # ------------------------------------------------------------------
    # Launch one service
    # ------------------------------------------------------------------

    async def _launch(self, svc: ManagedService) -> None:
        binary_path = self._resolve_binary(svc.manifest)
        if binary_path is None:
            svc.status = ServiceStatus.NO_BINARY
            raise RuntimeError(
                f"no binary for platform {self._platform!r} in {svc.name}/service.json"
            )
        if not binary_path.exists():
            svc.status = ServiceStatus.NO_BINARY
            raise RuntimeError(
                f"binary not found (not compiled?): {binary_path}"
            )

        socket_path = self._resolve_socket(svc.manifest)
        socket_path.parent.mkdir(parents=True, exist_ok=True)
        socket_path.unlink(missing_ok=True)

        env = {**os.environ, "OPENAGENT_SOCKET_PATH": str(socket_path)}
        env.update(self._env_extras.get(svc.name, {}))
        svc.status = ServiceStatus.STARTING

        process = await asyncio.create_subprocess_exec(
            str(binary_path),
            env=env,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT,
        )
        svc._process = process
        log_event(
            logger,
            logging.INFO,
            "service process spawned",
            component="service_manager",
            operation="launch",
            service=svc.name,
            binary=str(binary_path),
            pid=process.pid,
            socket=str(socket_path),
        )

        # Forward stdout to Python logger in background
        asyncio.create_task(
            self._pipe_stdout(svc, process),
            name=f"svcmgr.stdout.{svc.name}",
        )

        await self._wait_for_socket(svc, socket_path)

        client = McpLiteClient(socket_path=socket_path)
        await client.start()
        svc.client = client
        svc.status = ServiceStatus.RUNNING
        log_event(
            logger,
            logging.INFO,
            "service ready",
            component="service_manager",
            operation="launch",
            service=svc.name,
            socket=str(socket_path),
        )
        if self._service_ready_cb is not None:
            asyncio.create_task(
                self._service_ready_cb(svc.name),
                name=f"svcmgr.ready_cb.{svc.name}",
            )

    async def _wait_for_socket(self, svc: ManagedService, socket_path: Path) -> None:
        loop = asyncio.get_running_loop()
        deadline = loop.time() + _SOCKET_WAIT_S
        while loop.time() < deadline:
            if socket_path.exists():
                return
            if svc._process and svc._process.returncode is not None:
                raise RuntimeError(
                    f"process exited with code {svc._process.returncode} "
                    f"before socket appeared at {socket_path}"
                )
            await asyncio.sleep(_SOCKET_POLL_S)
        raise RuntimeError(
            f"socket {socket_path} did not appear within {_SOCKET_WAIT_S}s"
        )

    async def _pipe_stdout(
        self, svc: ManagedService, process: asyncio.subprocess.Process
    ) -> None:
        if process.stdout is None:
            return
        while True:
            line = await process.stdout.readline()
            if not line:
                break
            logger.info("[%s] %s", svc.name, line.decode("utf-8", errors="replace").rstrip())

    async def _teardown(self, svc: ManagedService) -> None:
        if svc.client:
            try:
                await svc.client.stop()
            except Exception:
                pass
            svc.client = None
        if svc._process and svc._process.returncode is None:
            try:
                svc._process.terminate()
                await asyncio.wait_for(svc._process.wait(), timeout=5.0)
            except Exception:
                log_event(
                    logger,
                    logging.WARNING,
                    "graceful termination timed out, escalating to SIGKILL",
                    component="service_manager",
                    operation="teardown",
                    service=svc.name,
                    pid=svc._process.pid,
                )
                try:
                    svc._process.kill()
                    await asyncio.wait_for(svc._process.wait(), timeout=2.0)
                except Exception as exc:
                    log_event(
                        logger,
                        logging.ERROR,
                        "SIGKILL failed",
                        component="service_manager",
                        operation="teardown",
                        service=svc.name,
                        error=str(exc),
                    )
        svc._process = None

    # ------------------------------------------------------------------
    # Manifest discovery and path resolution
    # ------------------------------------------------------------------

    def _discover_manifests(self) -> list[ServiceManifest]:
        services_dir = self._root / "services"
        manifests: list[ServiceManifest] = []
        if not services_dir.exists():
            return manifests
        for path in sorted(services_dir.glob("*/service.json")):
            try:
                data = json.loads(path.read_text(encoding="utf-8"))
            except Exception as exc:
                log_event(
                    logger,
                    logging.WARNING,
                    "failed to load service manifest",
                    component="service_manager",
                    operation="discover",
                    path=str(path),
                    error=str(exc),
                )
                continue
            manifests.append(ServiceManifest.from_dict(data, path))
        log_event(
            logger,
            logging.INFO,
            "discovered service manifests",
            component="service_manager",
            operation="discover",
            count=len(manifests),
            names=[m.name for m in manifests],
        )
        return manifests

    def _resolve_binary(self, manifest: ServiceManifest) -> Path | None:
        rel = manifest.binary.get(self._platform)
        if not rel:
            return None
        path = Path(rel)
        if path.is_absolute():
            return path
        # Relative to project root; Makefile builds to <root>/bin/
        return self._root / path

    def _resolve_socket(self, manifest: ServiceManifest) -> Path:
        p = Path(manifest.socket)
        if p.is_absolute():
            return p
        return self._root / p
