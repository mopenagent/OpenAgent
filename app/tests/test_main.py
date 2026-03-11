from __future__ import annotations

from unittest.mock import AsyncMock

from fastapi.testclient import TestClient

import app.main as app_main

app = app_main.app


def test_app_metadata() -> None:
    assert app.title == "OpenAgent"


def test_metrics_endpoint() -> None:
    class _FakeServiceManager:
        def __init__(self, *args, **kwargs) -> None:
            self._cb = None
            self._client = AsyncMock()

        async def start(self) -> None:
            return None

        async def stop(self) -> None:
            return None

        def on_service_ready(self, cb) -> None:
            self._cb = cb

        def get_client(self, name: str):
            return self._client if name == "cortex" else None

        def list_services(self):
            return []

    original = app_main.ServiceManager
    app_main.ServiceManager = _FakeServiceManager
    try:
        with TestClient(app) as client:
            resp = client.get("/metrics")
            assert hasattr(client.app.state, "heartbeat")
            assert client.app.state.heartbeat.last_snapshot is not None
    finally:
        app_main.ServiceManager = original
    assert resp.status_code == 200
    assert "openagent_" in resp.text
