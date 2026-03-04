from __future__ import annotations

from fastapi.testclient import TestClient

from app.main import app


def test_app_metadata() -> None:
    assert app.title == "OpenAgent"


def test_metrics_endpoint() -> None:
    with TestClient(app) as client:
        resp = client.get("/metrics")
    assert resp.status_code == 200
    assert "openagent_" in resp.text
