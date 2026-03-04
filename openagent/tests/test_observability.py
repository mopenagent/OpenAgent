from __future__ import annotations

import logging

from openagent.observability import configure_logging
from openagent.observability.metrics import render_metrics


def test_configure_logging_sets_root_handler() -> None:
    root = logging.getLogger()
    previous = list(root.handlers)
    try:
        configure_logging(force=True)
        assert len(root.handlers) >= 1
    finally:
        root.handlers = previous


def test_render_metrics_returns_prometheus_payload() -> None:
    payload, content_type = render_metrics()
    assert b"openagent_" in payload
    assert "text/plain" in content_type
