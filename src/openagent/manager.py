"""Extension discovery and initialization."""

from __future__ import annotations

import importlib.metadata
import inspect
from dataclasses import dataclass
from typing import List

from .interfaces import AsyncExtension

ENTRYPOINT_GROUP = "openagent.extensions"


@dataclass
class LoadedExtension:
    name: str
    instance: AsyncExtension


def _ensure_async_extension_contract(instance: object, name: str) -> AsyncExtension:
    init = getattr(instance, "initialize", None)
    shutdown = getattr(instance, "shutdown", None)
    if not callable(init) or not inspect.iscoroutinefunction(init):
        raise TypeError(f"Extension '{name}' must define async initialize(self) -> None.")
    if not callable(shutdown) or not inspect.iscoroutinefunction(shutdown):
        raise TypeError(f"Extension '{name}' must define async shutdown(self) -> None.")
    if not isinstance(instance, AsyncExtension):
        raise TypeError(f"Extension '{name}' does not match AsyncExtension protocol.")
    return instance


async def load_extensions() -> List[LoadedExtension]:
    """Discover and initialize installed OpenAgent extensions."""
    loaded: List[LoadedExtension] = []
    entry_points = importlib.metadata.entry_points()
    if hasattr(entry_points, "select"):
        entries = entry_points.select(group=ENTRYPOINT_GROUP)
    else:
        entries = entry_points.get(ENTRYPOINT_GROUP, [])

    for entry in entries:
        extension_class = entry.load()
        instance = _ensure_async_extension_contract(extension_class(), entry.name)
        await instance.initialize()
        loaded.append(LoadedExtension(name=entry.name, instance=instance))
        print(f"Loading First-Class Extension: {entry.name}")

    if not loaded:
        print(f"No extensions found in group '{ENTRYPOINT_GROUP}'.")

    return loaded


# Backward-compatible alias; prefer `load_extensions`.
load_plugins = load_extensions
