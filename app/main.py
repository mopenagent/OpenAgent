"""OpenAgent web UI — FastAPI application."""

from __future__ import annotations

import asyncio
import logging
from contextlib import asynccontextmanager
from pathlib import Path

from fastapi import FastAPI, Response
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates
from app.routes import dashboard, chat, services, config, provider, settings, browser, logs
from openagent.agent.platform_tools import make_platform_tools
from openagent.agent.skill_tools import make_skill_tools
from openagent.agent.loop import AgentLoop
from openagent.agent.middlewares.stt import STTMiddleware
from openagent.agent.middlewares.tts import TTSMiddleware
from openagent.agent.middlewares.whitelist import WhitelistMiddleware
from openagent.agent.tools import ToolRegistry
from openagent.session.browser import BrowserSessionManager
from openagent.bus.bus import MessageBus
from openagent.platforms.manager import PlatformManager
from openagent.platforms.web import WebPlatformAdapter
from openagent.config import build_service_env_extras, load_config
from openagent.heartbeat import (
    HeartbeatService,
    heartbeat_enabled_from_env,
    heartbeat_interval_from_env,
)
from openagent.cron import CronService, CronJob
from openagent.bus.events import InboundMessage, SenderInfo
from openagent.observability import configure_logging, setup_otel, shutdown_otel
from openagent.observability.metrics import render_metrics
from openagent.providers import CortexProvider
from openagent.services.manager import ServiceManager
from openagent.session import SessionManager, SqliteSessionBackend
from openagent.session.settings import SettingsStore

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

ROOT = Path(__file__).resolve().parents[1]
STATIC_DIR = Path(__file__).resolve().parent / "static"
TEMPLATES_DIR = Path(__file__).resolve().parent / "templates"
_CORTEX_START_TIMEOUT_S = 10.0

# ---------------------------------------------------------------------------
# Lifespan
# ---------------------------------------------------------------------------


@asynccontextmanager
async def lifespan(app: FastAPI):
    configure_logging(force=True)
    setup_otel(service_name="openagent", logs_dir=ROOT / "logs")
    logging.getLogger("openagent").info("Web UI started")

    app.state.root = ROOT

    # Full config — provider, agents, session, platforms, tools
    cfg = load_config(ROOT / "config" / "openagent.yaml")
    app.state.config = cfg

    # Settings store — persistent key-value in openagent.db
    db_path = ROOT / cfg.session.db_path  # same file as sessions (openagent.db)
    settings_store = SettingsStore(db_path)
    await settings_store.start()
    app.state.settings_store = settings_store

    app.state.provider_config = cfg.provider

    # Connector enable/disable — loaded from SQLite on each request; in-memory map
    # is populated lazily by the settings route when connectors are toggled.
    app.state.connectors_enabled = {}

    heartbeat = HeartbeatService(
        root=ROOT,
        enabled=heartbeat_enabled_from_env(),
        interval_s=heartbeat_interval_from_env(),
        provider_config_path=ROOT / "config" / "openagent.yaml",
    )
    app.state.heartbeat = heartbeat
    await heartbeat.start()

    # Build the set of services the operator has explicitly disabled so the
    # ServiceManager skips them on startup (start-then-stop is wasteful and
    # can leave sockets in a bad state).  Two sources are merged:
    #   1. service_state.enabled=0  (written by Services page Start/Stop buttons)
    #   2. settings connector.<name>.enabled=0  (written by Settings connectors toggle)
    _disabled: set[str] = set()
    _svc_states = await settings_store.get_all_service_states()
    for _svc_name, _svc_st in _svc_states.items():
        if not _svc_st["enabled"]:
            _disabled.add(_svc_name)
    _connector_settings = await settings_store.get_all(prefix="connector.")
    for _key, _val in _connector_settings.items():
        if _key.endswith(".enabled") and _val == "0":
            _disabled.add(_key.split(".")[1])

    # Service manager — inject platform credentials as env vars into Go services
    service_manager = ServiceManager(
        root=ROOT,
        env_extras=build_service_env_extras(cfg, ROOT),
        state_store=settings_store,
        disabled_names=frozenset(_disabled),
    )
    app.state.service_manager = service_manager
    await service_manager.start()
    cortex_client_getter = lambda: service_manager.get_client("cortex")
    cortex_ready = False
    started = asyncio.get_running_loop().time()
    while asyncio.get_running_loop().time() - started < _CORTEX_START_TIMEOUT_S:
        if cortex_client_getter() is not None:
            cortex_ready = True
            break
        await asyncio.sleep(0.1)
    if not cortex_ready:
        raise RuntimeError("cortex service did not become ready during startup")
    app.state.active_provider = CortexProvider(
        get_client=cortex_client_getter,
        default_agent_name=cfg.default_agent.name,
    )

    # Message bus
    bus = MessageBus()
    app.state.bus = bus
    await bus.start()

    async def on_cron_job(job: CronJob) -> None:
        msg = InboundMessage(
            platform="cron",
            sender=SenderInfo("cron", "system"),
            channel_id=job.payload.channel or "cron_default",
            content=job.payload.message,
            session_key_override=job.payload.to,
        )
        await bus.publish(msg)

    cron = CronService(
        store_path=ROOT / "data" / "cron.json",
        on_job=on_cron_job,
    )
    app.state.cron = cron
    await cron.start()

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

    # Browser session manager — one Chromium context per agent session, with a
    # 10-minute idle reaper that calls browser.close on stale contexts.
    browser_sessions = BrowserSessionManager(session_backend)
    app.state.browser_sessions = browser_sessions

    # Tool registry — tools are registered per-service as each comes online.
    # The on_service_ready callback fires from the watchdog after every successful
    # launch (initial start and restarts), so tools appear incrementally.
    tool_registry = ToolRegistry(service_manager, browser_sessions=browser_sessions)
    service_manager.on_service_ready(tool_registry.register_service)
    for name, description, params, fn in make_platform_tools(bus):
        tool_registry.register_native(name, description, params, fn)
    for name, description, params, fn in make_skill_tools():
        tool_registry.register_native(name, description, params, fn)

    # Start idle-browser reaper — closes Chromium contexts inactive for 10 min.
    browser_reaper_task = browser_sessions.start_reaper(tool_registry.call)
    app.state.browser_reaper_task = browser_reaper_task

    # Middleware — STT and TTS delegate to the Rust service daemons via ToolRegistry.
    # Returns empty string when the service is offline; middleware skips gracefully.

    async def _stt_fn(audio_path: str) -> str:
        import json as _json
        result = await tool_registry.call("stt.transcribe", {"audio_path": audio_path})
        try:
            return _json.loads(result).get("text", "") if result else ""
        except Exception:
            return ""

    async def _tts_fn(text: str) -> str:
        import json as _json
        result = await tool_registry.call("tts.synthesize", {"text": text})
        try:
            return _json.loads(result).get("path", "") if result else ""
        except Exception:
            return ""

    # Whitelist middleware — only active when whitelist.enabled = true in config
    _middlewares = []
    if cfg.whitelist.enabled:
        whitelist_mw = WhitelistMiddleware(backend=session_backend)
        await whitelist_mw.start()
        app.state.whitelist_middleware = whitelist_mw
        _middlewares.append(whitelist_mw)
    else:
        app.state.whitelist_middleware = None

    _middlewares += [
        STTMiddleware(stt_fn=_stt_fn),
        TTSMiddleware(tts_fn=_tts_fn),
    ]

    agent_loop = AgentLoop(
        bus=bus,
        provider=app.state.active_provider,
        sessions=session_manager,
        tools=tool_registry,
        system_prompt=cfg.default_agent.system_prompt,
        max_iterations=cfg.default_agent.max_iterations,
        max_tool_output=cfg.default_agent.max_tool_output,
        middlewares=_middlewares,
    )
    app.state.agent_loop = agent_loop
    await agent_loop.start()

    # Platform manager — auto-attaches adapters; uses session_manager for identity resolution
    def _get_connectors_enabled():
        return getattr(app.state, "connectors_enabled", {})

    platform_manager = PlatformManager(
        service_manager=service_manager,
        bus=bus,
        session_manager=session_manager,
        get_connectors_enabled=_get_connectors_enabled,
    )
    app.state.platform_manager = platform_manager

    # Web platform — pure-Python adapter for the browser /chat page
    web_platform = WebPlatformAdapter()
    app.state.web_platform = web_platform
    platform_manager.register(web_platform)

    await platform_manager.start()

    yield

    await platform_manager.stop()
    browser_reaper_task.cancel()
    await asyncio.gather(browser_reaper_task, return_exceptions=True)
    await agent_loop.stop()
    await session_manager.stop()
    await settings_store.stop()
    await bus.close()
    await service_manager.stop()
    await cron.stop()
    await heartbeat.stop()
    shutdown_otel()


# ---------------------------------------------------------------------------
# App
# ---------------------------------------------------------------------------

app = FastAPI(title="OpenAgent", lifespan=lifespan)

app.mount("/static", StaticFiles(directory=STATIC_DIR), name="static")

templates = Jinja2Templates(directory=str(TEMPLATES_DIR))

# Share templates with routes
dashboard.templates = templates
chat.templates = templates
services.templates = templates
config.templates = templates
settings.templates = templates
browser.templates = templates
logs.templates = templates

app.include_router(dashboard.router)
app.include_router(chat.router)
app.include_router(services.router)
app.include_router(config.router)
app.include_router(settings.router)
app.include_router(provider.router)
app.include_router(browser.router)
app.include_router(logs.router)


@app.get("/metrics")
async def metrics() -> Response:
    payload, content_type = render_metrics()
    return Response(content=payload, media_type=content_type)
