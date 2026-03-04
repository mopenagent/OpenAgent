from __future__ import annotations

from app.main import app


def test_app_metadata() -> None:
    assert app.title == "OpenAgent"
