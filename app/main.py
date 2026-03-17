"""OpenAgent web UI — FastAPI application.

Thin UI shell — all agent/service logic runs in the Rust openagent binary.
This process owns only:
  - Web chat WebSocket (calls Rust POST /step)
  - Session history reads (diary markdown files written by Rust cortex)
  - Settings persistence (SettingsStore in SQLite)
  - Cron scheduling (fires POST /step on the Rust API)
  - Observability (OTEL logs to logs/)
"""

from __future__ import annotations

import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path

import httpx
from fastapi import FastAPI, Response
from fastapi.staticfiles import StaticFiles
from fastapi.templating import Jinja2Templates
from app.routes import dashboard, chat, config, provider, settings, browser, logs, diary
from openagent.config import load_config
from openagent.cron import CronService, CronJob
from openagent.observability import configure_logging, setup_otel, shutdown_otel
from openagent.observability.metrics import render_metrics
from app.diary_store import DiaryStore

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

ROOT = Path(__file__).resolve().parents[1]
STATIC_DIR = Path(__file__).resolve().parent / "static"
TEMPLATES_DIR = Path(__file__).resolve().parent / "templates"

# Rust openagent binary — all agent/service operations go through this URL.
OPENAGENT_API_URL = os.environ.get("OPENAGENT_API_URL", "http://localhost:8080")

# ---------------------------------------------------------------------------
# Lifespan
# ---------------------------------------------------------------------------


@asynccontextmanager
async def lifespan(app: FastAPI):
    configure_logging(force=True)
    setup_otel(service_name="openagent", logs_dir=ROOT / "logs")
    logging.getLogger("openagent").info("Web UI started")

    app.state.root = ROOT

    # Full config — still loaded for settings/config pages and provider tab.
    cfg = load_config(ROOT / "config" / "openagent.yaml")
    app.state.config = cfg
    app.state.provider_config = cfg.provider

    # Connector enable/disable — in-memory map for Settings page.
    app.state.connectors_enabled = {}

    # Diary store — chat history from cortex diary markdown files.
    # No SQLite turns table needed; cortex writes diary on every turn.
    diary_root = ROOT / "data" / "diary"
    db_path = ROOT / cfg.session.db_path
    app.state.diary_store = DiaryStore(diary_root=diary_root, db_path=db_path)

    # Rust API client — web chat and cron use this to reach the Rust binary.
    api_client = httpx.AsyncClient(
        base_url=OPENAGENT_API_URL,
        timeout=httpx.Timeout(130.0),
    )
    app.state.api_client = api_client

    # Cron service — fires scheduled jobs by calling Rust POST /step.
    async def on_cron_job(job: CronJob) -> None:
        channel = job.payload.channel or "cron_default"
        session_id = job.payload.to or f"cron:{channel}"
        try:
            await api_client.post("/step", json={
                "platform": "cron",
                "channel_id": channel,
                "session_id": session_id,
                "user_input": job.payload.message,
            })
        except Exception as e:
            logging.getLogger("openagent").warning("cron.step.error: %s", e)

    cron = CronService(
        store_path=ROOT / "data" / "cron.json",
        on_job=on_cron_job,
    )
    app.state.cron = cron
    await cron.start()

    yield

    await cron.stop()
    await api_client.aclose()
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
config.templates = templates
settings.templates = templates
browser.templates = templates
logs.templates = templates
diary.templates = templates

app.include_router(dashboard.router)
app.include_router(chat.router)
app.include_router(config.router)
app.include_router(settings.router)
app.include_router(provider.router)
app.include_router(browser.router)
app.include_router(logs.router)
app.include_router(diary.router)


@app.get("/metrics")
async def metrics() -> Response:
    payload, content_type = render_metrics()
    return Response(content=payload, media_type=content_type)
