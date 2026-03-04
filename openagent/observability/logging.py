"""Structured logging utilities for OpenAgent."""

from __future__ import annotations

import json
import logging
import os
from datetime import datetime, timezone
from typing import Any

from .context import get_request_id


class JsonFormatter(logging.Formatter):
    """Emit deterministic JSON logs with stable fields."""

    def format(self, record: logging.LogRecord) -> str:
        payload: dict[str, Any] = {
            "ts": datetime.fromtimestamp(record.created, tz=timezone.utc).isoformat(),
            "level": record.levelname,
            "logger": record.name,
            "message": record.getMessage(),
        }

        request_id = get_request_id()
        if request_id:
            payload["request_id"] = request_id

        extra = getattr(record, "openagent_extra", None)
        if isinstance(extra, dict):
            payload.update(extra)

        if record.exc_info:
            payload["exception"] = self.formatException(record.exc_info)

        return json.dumps(payload, separators=(",", ":"), ensure_ascii=True)


class PlainFormatter(logging.Formatter):
    """Human-readable fallback formatter with request id support."""

    def format(self, record: logging.LogRecord) -> str:
        request_id = get_request_id() or "-"
        base = f"{self.formatTime(record)} {record.levelname:<8} {record.name} [{request_id}] {record.getMessage()}"
        if record.exc_info:
            return f"{base}\n{self.formatException(record.exc_info)}"
        return base


def configure_logging(*, force: bool = False) -> None:
    """Configure root logging for structured observability."""

    root = logging.getLogger()
    if root.handlers and not force:
        return

    json_enabled = os.getenv("OPENAGENT_LOG_JSON", "1").lower() not in {"0", "false", "no"}
    level_name = os.getenv("OPENAGENT_LOG_LEVEL", "INFO").upper()
    level = getattr(logging, level_name, logging.INFO)

    handler = logging.StreamHandler()
    if json_enabled:
        handler.setFormatter(JsonFormatter())
    else:
        handler.setFormatter(PlainFormatter(datefmt="%Y-%m-%dT%H:%M:%S"))

    root.handlers.clear()
    root.setLevel(level)
    root.addHandler(handler)


def get_logger(name: str) -> logging.Logger:
    return logging.getLogger(name)


def log_event(logger: logging.Logger, level: int, message: str, **fields: Any) -> None:
    logger.log(level, message, extra={"openagent_extra": fields})
