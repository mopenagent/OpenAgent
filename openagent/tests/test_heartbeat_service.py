from __future__ import annotations

import asyncio
import json
from pathlib import Path

import pytest

from openagent.heartbeat import HeartbeatService


@pytest.mark.asyncio
async def test_heartbeat_tick_reports_offline_service(tmp_path: Path) -> None:
    services_dir = tmp_path / "services" / "demo"
    services_dir.mkdir(parents=True)
    (services_dir / "service.json").write_text(
        json.dumps(
            {
                "name": "demo",
                "version": "0.1.0",
                "socket": "data/sockets/demo.sock",
                "tools": [{"name": "demo.echo", "description": "", "params": {}}],
                "events": ["demo.event"],
            }
        ),
        encoding="utf-8",
    )

    cfg_dir = tmp_path / "config"
    cfg_dir.mkdir(parents=True)
    (cfg_dir / "openagent.yaml").write_text(
        "provider:\n  kind: openai_compat\n  model: test-model\n",
        encoding="utf-8",
    )

    hb = HeartbeatService(
        root=tmp_path,
        interval_s=2,
        enabled=True,
        provider_config_path=cfg_dir / "openagent.yaml",
    )

    snapshot = await hb.tick()
    assert snapshot.services_total == 1
    assert snapshot.services_online == 0
    assert snapshot.services_offline == 1
    assert snapshot.provider["kind"] == "openai_compat"
    assert snapshot.provider["model"] == "test-model"
    assert snapshot.services[0].name == "demo"
    assert snapshot.services[0].status == "offline"


@pytest.mark.asyncio
async def test_poll_service_online_with_mocked_socket(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    class _FakeReader:
        async def readline(self) -> bytes:
            return b'{"id":"abc","type":"pong","status":"ready"}\n'

    class _FakeWriter:
        def write(self, _data: bytes) -> None:
            return None

        async def drain(self) -> None:
            return None

        def close(self) -> None:
            return None

        async def wait_closed(self) -> None:
            return None

    async def _fake_open_unix_connection(_path: str):
        return _FakeReader(), _FakeWriter()

    monkeypatch.setattr(asyncio, "open_unix_connection", _fake_open_unix_connection)

    hb = HeartbeatService(root=tmp_path)
    service = await hb._poll_service(
        {
            "name": "mock",
            "version": "0.1.0",
            "socket": "data/sockets/mock.sock",
            "tools": [],
            "events": [],
            "health": {"timeout_ms": 100},
        }
    )
    assert service.status == "online"
    assert service.error is None
