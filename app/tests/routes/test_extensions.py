from __future__ import annotations

from types import SimpleNamespace

from app.routes import extensions


def test_get_extensions_lists_registered(monkeypatch) -> None:
    entry = SimpleNamespace(name="demo", value="demo_pkg:DemoExtension")

    class _Dist:
        metadata = {"Version": "1.2.3", "Name": "demo_pkg"}

    monkeypatch.setattr(extensions.importlib.metadata, "entry_points", lambda group: [entry])
    monkeypatch.setattr(extensions.importlib.metadata, "distribution", lambda _pkg: _Dist())

    result = extensions._get_extensions()
    assert len(result) == 1
    assert result[0]["name"] == "demo"
    assert result[0]["version"] == "1.2.3"
    assert result[0]["status"] == "registered"
