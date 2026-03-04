from __future__ import annotations

import asyncio
from types import SimpleNamespace

from openagent import manager


class _Extension:
    def __init__(self):
        self.started = False

    async def initialize(self):
        self.started = True

    async def shutdown(self):
        return None

    def get_status(self):
        return {}


class _InvalidExtension:
    def initialize(self):
        return None


def test_load_extensions_empty(monkeypatch, capsys):
    monkeypatch.setattr(manager.importlib.metadata, "entry_points", lambda: {})
    loaded = asyncio.run(manager.load_extensions())
    out = capsys.readouterr().out
    assert loaded == []
    assert "No extensions found" in out


def test_load_extensions_select_api(monkeypatch):
    entry = SimpleNamespace(name="demo", load=lambda: _Extension)

    class _EntryPoints:
        @staticmethod
        def select(group):
            assert group == manager.ENTRYPOINT_GROUP
            return [entry]

    monkeypatch.setattr(manager.importlib.metadata, "entry_points", lambda: _EntryPoints())
    loaded = asyncio.run(manager.load_extensions())
    assert len(loaded) == 1
    assert loaded[0].name == "demo"
    assert loaded[0].instance.started is True


def test_load_extensions_rejects_sync_extensions(monkeypatch):
    entry = SimpleNamespace(name="bad", load=lambda: _InvalidExtension)

    class _EntryPoints:
        @staticmethod
        def select(group):
            assert group == manager.ENTRYPOINT_GROUP
            return [entry]

    monkeypatch.setattr(manager.importlib.metadata, "entry_points", lambda: _EntryPoints())
    try:
        asyncio.run(manager.load_extensions())
    except TypeError as exc:
        assert "async initialize" in str(exc)
    else:  # pragma: no cover
        raise AssertionError("Expected async contract enforcement failure")


def test_load_plugins_alias_kept_for_compatibility(monkeypatch):
    entry = SimpleNamespace(name="demo", load=lambda: _Extension)

    class _EntryPoints:
        @staticmethod
        def select(group):
            assert group == manager.ENTRYPOINT_GROUP
            return [entry]

    monkeypatch.setattr(manager.importlib.metadata, "entry_points", lambda: _EntryPoints())
    loaded = asyncio.run(manager.load_plugins())
    assert len(loaded) == 1


def test_get_extension_lookup(monkeypatch):
    entry = SimpleNamespace(name="lookup", load=lambda: _Extension)

    class _EntryPoints:
        @staticmethod
        def select(group):
            assert group == manager.ENTRYPOINT_GROUP
            return [entry]

    monkeypatch.setattr(manager.importlib.metadata, "entry_points", lambda: _EntryPoints())
    asyncio.run(manager.load_extensions())
    assert manager.get_extension("lookup") is not None
