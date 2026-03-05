"""OpenAgent web UI — FastAPI application."""

from __future__ import annotations

import asyncio
import logging
from collections import deque
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, Response
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates

from app.routes import dashboard, chat, logs, extensions, services, config, llm, provider
from openagent.agent.identity_tools import make_identity_tools
from openagent.agent.loop import AgentLoop
from openagent.agent.tools import ToolRegistry
from openagent.bus.bus import MessageBus
from openagent.channels.manager import ChannelManager
from openagent.channels.web import WebChannelAdapter
from openagent.config import build_service_env_extras, load_config
from openagent.heartbeat import (
    HeartbeatService,
    heartbeat_enabled_from_env,
    heartbeat_interval_from_env,
)
from openagent.observability import configure_logging
from openagent.observability.metrics import render_metrics
from openagent.providers import get_provider
from openagent.services.manager import ServiceManager
from openagent.session import SessionManager, SqliteSessionBackend

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
    configure_logging(force=True)
    logging.getLogger().addHandler(_handler)
    for _uvi in ("uvicorn", "uvicorn.error", "uvicorn.access"):
        logging.getLogger(_uvi).addHandler(_handler)
    logging.getLogger("openagent").info("Web UI started")

    app.state.log_buffer = LOG_BUFFER
    app.state.log_clients = LOG_CLIENTS
    app.state.root = ROOT

    # Full config — provider, agents, session, channels, tools
    cfg = load_config(ROOT / "config" / "openagent.yaml")
    app.state.config = cfg
    app.state.provider_config = cfg.provider   # backward compat for provider route
    app.state.active_provider = get_provider(cfg.provider)

    heartbeat = HeartbeatService(
        root=ROOT,
        enabled=heartbeat_enabled_from_env(),
        interval_s=heartbeat_interval_from_env(),
        provider_config_path=ROOT / "config" / "openagent.yaml",
    )
    app.state.heartbeat = heartbeat
    await heartbeat.start()

    # Service manager — inject channel credentials as env vars into Go services
    service_manager = ServiceManager(
        root=ROOT,
        env_extras=build_service_env_extras(cfg),
    )
    app.state.service_manager = service_manager
    await service_manager.start()

    # Message bus
    bus = MessageBus()
    app.state.bus = bus
    await bus.start()

    # Session manager
    session_backend = SqliteSessionBackend(
        ROOT / cfg.session.db_path
    )
    session_manager = SessionManager(
        backend=session_backend,
        summarise_after=cfg.session.summarise_after,
    )
    app.state.session_manager = session_manager
    await session_manager.start()

    # Tool registry — Go service tools + Python-native identity tools
    tool_registry = ToolRegistry(service_manager)
    await tool_registry.rebuild()
    for name, description, params, fn in make_identity_tools(session_manager):
        tool_registry.register_native(name, description, params, fn)
    agent_loop = AgentLoop(
        bus=bus,
        provider=app.state.active_provider,
        sessions=session_manager,
        tools=tool_registry,
        system_prompt=cfg.default_agent.system_prompt,
    )
    app.state.agent_loop = agent_loop
    await agent_loop.start()

    # Channel manager — auto-attaches adapters; session_manager provides identity resolver
    channel_manager = ChannelManager(
        service_manager=service_manager,
        bus=bus,
        session_manager=session_manager,
    )
    app.state.channel_manager = channel_manager

    # Web channel — pure-Python adapter for the browser /chat page
    web_channel = WebChannelAdapter()
    app.state.web_channel = web_channel
    channel_manager.register(web_channel)

    await channel_manager.start()

    yield

    await channel_manager.stop()
    await agent_loop.stop()
    await session_manager.stop()
    await bus.close()
    await service_manager.stop()
    await heartbeat.stop()
    logging.getLogger().removeHandler(_handler)
    for _uvi in ("uvicorn", "uvicorn.error", "uvicorn.access"):
        logging.getLogger(_uvi).removeHandler(_handler)


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


@app.get("/metrics")
async def metrics() -> Response:
    payload, content_type = render_metrics()
    return Response(content=payload, media_type=content_type)
