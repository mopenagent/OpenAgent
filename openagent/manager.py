"""Extension discovery and initialization."""

from __future__ import annotations

import importlib.metadata
import inspect
import logging
from dataclasses import dataclass
from typing import List

from .interfaces import AsyncExtension
from .observability import get_logger, log_event
from .observability.metrics import EXTENSION_LIFECYCLE_TOTAL

ENTRYPOINT_GROUP = "openagent.extensions"
_LOADED_EXTENSIONS: dict[str, AsyncExtension] = {}
logger = get_logger(__name__)


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
        try:
            await instance.initialize()
        except Exception as exc:
            EXTENSION_LIFECYCLE_TOTAL.labels(
                extension=entry.name,
                operation="initialize",
                status="error",
            ).inc()
            log_event(
                logger,
                logging.ERROR,
                "extension initialize failed",
                component="extension.manager",
                operation="initialize",
                extension=entry.name,
                error=str(exc),
            )
            raise

        EXTENSION_LIFECYCLE_TOTAL.labels(
            extension=entry.name,
            operation="initialize",
            status="ok",
        ).inc()
        _LOADED_EXTENSIONS[entry.name] = instance
        loaded.append(LoadedExtension(name=entry.name, instance=instance))
        log_event(
            logger,
            logging.INFO,
            "extension initialized",
            component="extension.manager",
            operation="initialize",
            extension=entry.name,
            status="ok",
        )

    if not loaded:
        log_event(
            logger,
            logging.INFO,
            "no extensions discovered",
            component="extension.manager",
            operation="discover",
            entrypoint_group=ENTRYPOINT_GROUP,
        )

    return loaded


def get_extension(name: str) -> AsyncExtension | None:
    """Return a loaded extension instance by entry-point name."""
    return _LOADED_EXTENSIONS.get(name)


# Backward-compatible alias; prefer `load_extensions`.
load_plugins = load_extensions
