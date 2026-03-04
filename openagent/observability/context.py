"""Request context helpers for correlated logging and metrics."""

from __future__ import annotations

from contextvars import ContextVar
import uuid

_request_id_var: ContextVar[str | None] = ContextVar("openagent_request_id", default=None)


def get_request_id() -> str | None:
    return _request_id_var.get()


def set_request_id(request_id: str | None) -> None:
    _request_id_var.set(request_id)


def ensure_request_id() -> str:
    value = _request_id_var.get()
    if value:
        return value
    value = str(uuid.uuid4())
    _request_id_var.set(value)
    return value
