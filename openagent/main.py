"""OpenAgent runtime entry point."""

from __future__ import annotations

import asyncio

from .manager import load_extensions
from .observability import configure_logging, get_logger


logger = get_logger(__name__)


def run() -> None:
    configure_logging()
    logger.info("openagent runtime starting")
    asyncio.run(load_extensions())


if __name__ == "__main__":
    run()
