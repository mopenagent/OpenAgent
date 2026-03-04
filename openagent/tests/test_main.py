from __future__ import annotations

from openagent import main


def test_run_invokes_extension_loader(monkeypatch):
    called = {"value": False}

    async def fake_load_extensions():
        called["value"] = True
        return []

    monkeypatch.setattr(main, "load_extensions", fake_load_extensions)
    main.run()
    assert called["value"] is True
