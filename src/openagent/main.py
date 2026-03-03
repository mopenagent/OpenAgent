"""OpenAgent runtime entry point."""

from __future__ import annotations

import asyncio

from .manager import load_extensions


def run() -> None:
    asyncio.run(load_extensions())


if __name__ == "__main__":
    run()
