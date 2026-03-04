"""Tests for openagent.services.manager — ServiceManager."""

from __future__ import annotations

import asyncio
import json
import sys
from pathlib import Path

import pytest

from openagent.services.manager import (
    ManagedService,
    ServiceManifest,
    ServiceManager,
    ServiceStatus,
    _current_platform,
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _make_manifest(tmp_path: Path, name: str = "testsvc", extra: dict | None = None) -> Path:
    svc_dir = tmp_path / "services" / name
    svc_dir.mkdir(parents=True)
    data: dict = {
        "name": name,
        "description": "Test service",
        "version": "0.1.0",
        "binary": {},
        "socket": f"data/sockets/{name}.sock",
        "health": {"interval_ms": 5000, "timeout_ms": 1000, "restart_backoff_ms": [100, 200]},
        "tools": [{"name": "t1", "description": "tool1", "params": {}}],
        "events": [],
    }
    if extra:
        data.update(extra)
    (svc_dir / "service.json").write_text(json.dumps(data))
    return svc_dir


# ---------------------------------------------------------------------------
# Unit tests — manifest loading
# ---------------------------------------------------------------------------


def test_discover_manifests_empty(tmp_path: Path) -> None:
    mgr = ServiceManager(root=tmp_path)
    manifests = mgr._discover_manifests()
    assert manifests == []


def test_discover_manifests_finds_services(tmp_path: Path) -> None:
    _make_manifest(tmp_path, "alpha")
    _make_manifest(tmp_path, "beta")
    mgr = ServiceManager(root=tmp_path)
    manifests = mgr._discover_manifests()
    names = [m.name for m in manifests]
    assert "alpha" in names
    assert "beta" in names


def test_discover_manifests_skips_bad_json(tmp_path: Path) -> None:
    svc_dir = tmp_path / "services" / "broken"
    svc_dir.mkdir(parents=True)
    (svc_dir / "service.json").write_text("NOT JSON{{")
    mgr = ServiceManager(root=tmp_path)
    manifests = mgr._discover_manifests()
    assert manifests == []


def test_manifest_from_dict_defaults(tmp_path: Path) -> None:
    manifest_path = tmp_path / "service.json"
    manifest_path.write_text("{}")
    m = ServiceManifest.from_dict({}, manifest_path)
    # name falls back to parent dir name
    assert m.name == tmp_path.name
    assert m.version == "0.0.0"
    assert m.tools == []
    assert m.events == []
    assert m.health.restart_backoff_ms == [1000, 2000, 5000, 10000, 30000]


def test_manifest_from_dict_full(tmp_path: Path) -> None:
    manifest_path = tmp_path / "service.json"
    data = {
        "name": "hello",
        "description": "Greeter",
        "version": "1.2.3",
        "binary": {"darwin/arm64": "bin/hello-darwin-arm64"},
        "socket": "data/sockets/hello.sock",
        "health": {"interval_ms": 3000, "timeout_ms": 500, "restart_backoff_ms": [50, 100]},
        "tools": [{"name": "hello.reply", "description": "greet", "params": {}}],
        "events": [{"name": "ping"}],
    }
    m = ServiceManifest.from_dict(data, manifest_path)
    assert m.name == "hello"
    assert m.version == "1.2.3"
    assert m.health.interval_ms == 3000
    assert m.health.restart_backoff_ms == [50, 100]
    assert len(m.tools) == 1
    assert len(m.events) == 1


# ---------------------------------------------------------------------------
# Unit tests — binary and socket resolution
# ---------------------------------------------------------------------------


def test_resolve_binary_missing_platform_key(tmp_path: Path) -> None:
    _make_manifest(tmp_path, "svc", extra={"binary": {"other/arch": "bin/svc"}})
    mgr = ServiceManager(root=tmp_path)
    manifests = mgr._discover_manifests()
    assert len(manifests) == 1
    result = mgr._resolve_binary(manifests[0])
    assert result is None


def test_resolve_binary_current_platform(tmp_path: Path) -> None:
    platform_key = _current_platform()
    _make_manifest(tmp_path, "svc", extra={"binary": {platform_key: "bin/svc-bin"}})
    mgr = ServiceManager(root=tmp_path)
    manifests = mgr._discover_manifests()
    result = mgr._resolve_binary(manifests[0])
    assert result == tmp_path / "bin" / "svc-bin"


def test_resolve_socket_relative(tmp_path: Path) -> None:
    _make_manifest(tmp_path, "svc", extra={"socket": "data/sockets/svc.sock"})
    mgr = ServiceManager(root=tmp_path)
    manifests = mgr._discover_manifests()
    resolved = mgr._resolve_socket(manifests[0])
    assert resolved == tmp_path / "data" / "sockets" / "svc.sock"


def test_resolve_socket_absolute(tmp_path: Path) -> None:
    abs_path = str(tmp_path / "custom.sock")
    _make_manifest(tmp_path, "svc", extra={"socket": abs_path})
    mgr = ServiceManager(root=tmp_path)
    manifests = mgr._discover_manifests()
    resolved = mgr._resolve_socket(manifests[0])
    assert resolved == Path(abs_path)


# ---------------------------------------------------------------------------
# Unit tests — ManagedService
# ---------------------------------------------------------------------------


def test_managed_service_to_dict(tmp_path: Path) -> None:
    manifest_path = tmp_path / "service.json"
    m = ServiceManifest.from_dict(
        {"name": "svc", "version": "0.2.0", "tools": [{"name": "t", "description": "d", "params": {}}]},
        manifest_path,
    )
    svc = ManagedService(m)
    d = svc.to_dict()
    assert d["name"] == "svc"
    assert d["status"] == "stopped"
    assert d["tools"] == 1
    assert d["restart_count"] == 0


# ---------------------------------------------------------------------------
# Async tests — no_binary watchdog exits cleanly
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_no_binary_service_gets_no_binary_status(tmp_path: Path) -> None:
    """A service with no binary for the current platform enters NO_BINARY and stops."""
    _make_manifest(tmp_path, "svc", extra={"binary": {}})
    mgr = ServiceManager(root=tmp_path)
    await mgr.start()
    # Allow watchdog task to run
    await asyncio.sleep(0.1)
    services = mgr.list_services()
    assert len(services) == 1
    assert services[0].status in (ServiceStatus.NO_BINARY, ServiceStatus.STOPPED)
    await mgr.stop()


# ---------------------------------------------------------------------------
# Integration test — full lifecycle with Python mock service
# ---------------------------------------------------------------------------

# A minimal Python script that acts as a Go MCP-lite service:
# creates a Unix socket, handles ping/pong + tools.list, exits on SIGTERM.
_MOCK_SERVICE = """\
#!/usr/bin/env python3
import asyncio, json, os, signal, sys

async def handle(reader, writer):
    while True:
        line = await reader.readline()
        if not line:
            break
        try:
            frame = json.loads(line)
        except Exception:
            continue
        ft = frame.get("type")
        if ft == "ping":
            out = {"id": frame["id"], "type": "pong", "status": "ready"}
        elif ft == "tools.list":
            out = {"id": frame["id"], "type": "tools.list.ok", "tools": []}
        else:
            continue
        writer.write((json.dumps(out) + "\\n").encode())
        await writer.drain()

async def main():
    sock = os.environ.get("OPENAGENT_SOCKET_PATH", "/tmp/mock_svc_test.sock")
    server = await asyncio.start_unix_server(handle, sock)
    stop = asyncio.Event()
    loop = asyncio.get_event_loop()
    loop.add_signal_handler(signal.SIGTERM, stop.set)
    async with server:
        await stop.wait()

asyncio.run(main())
"""


@pytest.mark.asyncio
async def test_service_manager_full_lifecycle(tmp_path: Path) -> None:
    """ServiceManager launches a Python mock service, gets a live client, then stops cleanly."""
    # Write executable mock service script
    script = tmp_path / "mock_svc.py"
    script.write_text(f"#!{sys.executable}\n" + _MOCK_SERVICE)
    script.chmod(0o755)

    platform_key = _current_platform()
    # macOS Unix socket path limit is 104 chars; use /tmp/ to stay well under it
    socket_path = "/tmp/oa_test_mock.sock"

    _make_manifest(
        tmp_path,
        "mock",
        extra={
            "binary": {platform_key: str(script)},
            "socket": socket_path,
            "health": {"interval_ms": 5000, "timeout_ms": 500, "restart_backoff_ms": [50]},
        },
    )

    mgr = ServiceManager(root=tmp_path)
    await mgr.start()

    # Wait for service to become RUNNING
    for _ in range(40):
        services = mgr.list_services()
        if services and services[0].status == ServiceStatus.RUNNING:
            break
        await asyncio.sleep(0.1)
    else:
        await mgr.stop()
        pytest.fail(f"Service did not reach RUNNING; last status: {services[0].status}, error: {services[0].last_error}")

    services = mgr.list_services()
    assert services[0].status == ServiceStatus.RUNNING
    assert services[0].restart_count == 0

    client = mgr.get_client("mock")
    assert client is not None
    assert client.running

    await mgr.stop()

    services = mgr.list_services()
    assert services[0].status == ServiceStatus.STOPPED
    assert mgr.get_client("mock") is None


@pytest.mark.asyncio
async def test_service_manager_restarts_after_crash(tmp_path: Path) -> None:
    """Killing the process causes the watchdog to restart it (restart_count == 1)."""
    script = tmp_path / "mock_svc2.py"
    script.write_text(f"#!{sys.executable}\n" + _MOCK_SERVICE)
    script.chmod(0o755)

    platform_key = _current_platform()
    socket_path = "/tmp/oa_test_mock2.sock"

    _make_manifest(
        tmp_path,
        "mock2",
        extra={
            "binary": {platform_key: str(script)},
            "socket": socket_path,
            "health": {"interval_ms": 5000, "timeout_ms": 500, "restart_backoff_ms": [100]},
        },
    )

    mgr = ServiceManager(root=tmp_path)
    await mgr.start()

    async def _wait_running(count: int = 0) -> bool:
        for _ in range(40):
            svcs = mgr.list_services()
            if svcs and svcs[0].status == ServiceStatus.RUNNING and svcs[0].restart_count == count:
                return True
            await asyncio.sleep(0.1)
        return False

    assert await _wait_running(0), "service did not reach RUNNING"

    # Kill the process — watchdog should detect exit and restart
    svc = mgr.list_services()[0]
    assert svc._process is not None
    svc._process.terminate()

    # Wait for restart (restart_count becomes 1) and service to be RUNNING again
    assert await _wait_running(1), f"service did not restart; status={svc.status}, error={svc.last_error}"

    assert svc.status == ServiceStatus.RUNNING
    assert mgr.get_client("mock2") is not None

    await mgr.stop()
