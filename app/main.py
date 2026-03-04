"""OpenAgent web UI — FastAPI application."""

from __future__ import annotations

import asyncio
import logging
from collections import deque
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

from app.routes import dashboard, chat, logs, extensions, services, config, llm, provider
from openagent.providers import load_provider_config, get_provider

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

ROOT = Path(__file__).resolve().parents[1]
STATIC_DIR = Path(__file__).resolve().parent / "static"
TEMPLATES_DIR = Path(__file__).resolve().parent / "templates"

# ---------------------------------------------------------------------------
# Global log capture — rolling buffer + per-client SSE queues
# ---------------------------------------------------------------------------

LOG_BUFFER: deque[str] = deque(maxlen=500)
LOG_CLIENTS: set[asyncio.Queue[str]] = set()


class _SSELogHandler(logging.Handler):
    """Feeds Python log records into the SSE broadcast system."""

    def emit(self, record: logging.LogRecord) -> None:
        msg = self.format(record)
        LOG_BUFFER.append(msg)
        for q in LOG_CLIENTS:
            try:
                q.put_nowait(msg)
            except asyncio.QueueFull:
                pass


_handler = _SSELogHandler()
_handler.setFormatter(
    logging.Formatter("%(asctime)s %(levelname)-8s %(name)s  %(message)s",
                      datefmt="%H:%M:%S")
)

# ---------------------------------------------------------------------------
# Lifespan
# ---------------------------------------------------------------------------


@asynccontextmanager
async def lifespan(app: FastAPI):
    logging.getLogger().addHandler(_handler)
    logging.getLogger().setLevel(logging.DEBUG)
    logging.getLogger("openagent").info("Web UI started")
    app.state.log_buffer = LOG_BUFFER
    app.state.log_clients = LOG_CLIENTS
    app.state.root = ROOT
    provider_cfg = load_provider_config(ROOT / "config" / "openagent.yaml")
    app.state.provider_config = provider_cfg
    app.state.active_provider = get_provider(provider_cfg)
    yield
    logging.getLogger().removeHandler(_handler)


# ---------------------------------------------------------------------------
# App
# ---------------------------------------------------------------------------

app = FastAPI(title="OpenAgent", lifespan=lifespan)

app.mount("/static", StaticFiles(directory=STATIC_DIR), name="static")

templates = Jinja2Templates(directory=str(TEMPLATES_DIR))

# Share templates with routes
dashboard.templates = templates
chat.templates = templates
logs.templates = templates
extensions.templates = templates
services.templates = templates
config.templates = templates

app.include_router(dashboard.router)
app.include_router(chat.router)
app.include_router(logs.router)
app.include_router(extensions.router)
app.include_router(services.router)
app.include_router(config.router)
app.include_router(llm.router)
app.include_router(provider.router)
