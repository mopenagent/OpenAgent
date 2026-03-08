"""Settings route — GET /settings (Provider + Connector + Whitelist tabs)."""

from __future__ import annotations

import io
import base64

from fastapi import APIRouter, Request
from fastapi.templating import Jinja2Templates

router = APIRouter()
templates: Jinja2Templates  # injected by main.py


# ---------------------------------------------------------------------------
# Page
# ---------------------------------------------------------------------------

@router.get("/settings")
async def settings_page(request: Request):
    return templates.TemplateResponse("settings.html", {
        "request": request,
        "active": "settings",
    })


# ---------------------------------------------------------------------------
# Connectors API (enable/disable Discord, Slack, Telegram, WhatsApp)
# ---------------------------------------------------------------------------

CONNECTOR_INFO = {
    "discord":  {"description": "Discord bot for servers and DMs. Configure token in config/openagent.yaml."},
    "slack":    {"description": "Slack workspace bot. Requires bot token and app token."},
    "telegram": {"description": "Telegram bot or MTProto client. Configure app_id, app_hash, bot_token."},
    "whatsapp": {"description": "WhatsApp via whatsmeow (Go). Link phone via QR code below."},
}

_SETTINGS_KEY = "connector.{name}.enabled"


def _settings_store(request: Request):
    return getattr(request.app.state, "settings_store", None)


def _service_manager(request: Request):
    return getattr(request.app.state, "service_manager", None)


def _session_backend(request: Request):
    mgr = getattr(request.app.state, "session_manager", None)
    return mgr._backend if mgr and hasattr(mgr, "_backend") else None


def _whitelist_middleware(request: Request):
    return getattr(request.app.state, "whitelist_middleware", None)


@router.get("/api/settings/connectors")
async def list_connectors(request: Request):
    """Return connector list with enabled state (from SQLite) and running status."""
    store = _settings_store(request)
    mgr = _service_manager(request)

    # Load all connector settings in one query
    all_settings: dict[str, str] = {}
    if store:
        all_settings = await store.get_all(prefix="connector.")

    # Build running-status map from ServiceManager
    running: dict[str, bool] = {}
    if mgr:
        for svc in mgr.list_services():
            running[svc.name] = svc.status.value == "running"

    connectors = []
    for name, info in CONNECTOR_INFO.items():
        raw = all_settings.get(_SETTINGS_KEY.format(name=name))
        # Default: enabled if credentials exist in config (non-empty token)
        if raw is None:
            cfg = getattr(request.app.state, "config", None)
            platforms = getattr(cfg, "platforms", None) if cfg else None
            has_creds = bool(getattr(platforms, name, None)) if platforms else False
            enabled = has_creds
        else:
            enabled = raw == "1"

        connectors.append({
            "name": name,
            "description": info["description"],
            "enabled": enabled,
            "running": running.get(name, False),
        })
    return {"connectors": connectors}


@router.patch("/api/settings/connectors/{name}")
async def patch_connector(request: Request, name: str):
    """Enable or disable a connector — persists to SQLite and starts/stops the service."""
    body = await request.json()
    enabled = body.get("enabled")
    if enabled is None:
        return {"ok": False, "error": "enabled required"}
    if name not in CONNECTOR_INFO:
        return {"ok": False, "error": f"unknown connector: {name}"}

    store = _settings_store(request)
    mgr = _service_manager(request)

    # Persist to SQLite
    if store:
        await store.set(_SETTINGS_KEY.format(name=name), "1" if enabled else "0")
    # Also update in-memory map so PlatformManager picks it up immediately
    enabled_map = getattr(request.app.state, "connectors_enabled", {})
    enabled_map[name] = bool(enabled)
    request.app.state.connectors_enabled = enabled_map

    # Start or stop the Go service
    if mgr:
        if enabled:
            ok = await mgr.reload(name)
            return {"ok": ok, "action": "started"}
        else:
            ok = await mgr.stop_service(name)
            return {"ok": ok, "action": "stopped"}

    return {"ok": True}


# ---------------------------------------------------------------------------
# WhatsApp QR (for linking in Settings > Connector)
# ---------------------------------------------------------------------------


@router.get("/api/settings/whatsapp/qr")
async def whatsapp_qr(request: Request):
    """Return WhatsApp QR code as data URL for scanning. QR comes from the Go whatsapp service (whatsmeow)."""
    qr_text: str | None = None
    connected = False
    status = "unavailable"

    platform_manager = getattr(request.app.state, "platform_manager", None)
    if platform_manager:
        adapters = platform_manager.adapters()
        wa_adapter = adapters.get("whatsapp")
        if wa_adapter and hasattr(wa_adapter, "latest_qr"):
            qr_text = wa_adapter.latest_qr() or None
            connected = wa_adapter._status.get("connected", False)
            status = "connected" if connected else ("pending" if qr_text else "waiting")

    if not qr_text:
        if status == "unavailable":
            msg = (
                "WhatsApp service not available. Ensure the Go whatsapp service is built "
                "('make whatsapp') and running. Check Settings > Connector to enable WhatsApp."
            )
        elif connected:
            msg = "WhatsApp is already connected — no QR needed."
        else:
            msg = "Waiting for QR code… WhatsApp is starting up. Refresh in a few seconds."
        return {"qr": None, "connected": connected, "status": status, "message": msg}

    # Generate QR image as base64 data URL
    try:
        import qrcode
        img = qrcode.make(qr_text)
        buf = io.BytesIO()
        img.save(buf, format="PNG")
        buf.seek(0)
        b64 = base64.b64encode(buf.read()).decode("ascii")
        data_url = f"data:image/png;base64,{b64}"
        return {
            "qr": data_url,
            "connected": connected,
            "status": status,
            "message": "Scan with WhatsApp: Settings > Linked Devices > Link a Device",
        }
    except Exception as e:
        return {
            "qr": None,
            "connected": connected,
            "status": "error",
            "message": f"QR generation failed: {e}",
        }


# ---------------------------------------------------------------------------
# Whitelist API
# ---------------------------------------------------------------------------


@router.get("/api/settings/whitelist")
async def list_whitelist(request: Request):
    """Return all whitelist entries."""
    backend = _session_backend(request)
    if not backend:
        return {"entries": []}
    entries = await backend.get_whitelist()
    return {"entries": entries}


@router.post("/api/settings/whitelist")
async def add_whitelist_entry(request: Request):
    """Add a new whitelist entry."""
    body = await request.json()
    platform = body.get("platform", "").strip()
    channel_id = body.get("channel_id", "").strip()
    label = body.get("label", "").strip()
    added_by = body.get("added_by", "").strip()

    if not platform or not channel_id:
        return {"ok": False, "error": "platform and channel_id are required"}

    backend = _session_backend(request)
    if not backend:
        return {"ok": False, "error": "session backend not available"}

    await backend.add_to_whitelist(
        platform, channel_id, label=label, added_by=added_by
    )

    # Refresh in-memory cache if middleware is running
    mw = _whitelist_middleware(request)
    if mw:
        await mw.refresh()

    return {"ok": True}


@router.delete("/api/settings/whitelist/{platform}/{channel_id:path}")
async def remove_whitelist_entry(request: Request, platform: str, channel_id: str):
    """Remove a whitelist entry."""
    backend = _session_backend(request)
    if not backend:
        return {"ok": False, "error": "session backend not available"}

    await backend.remove_from_whitelist(platform, channel_id)

    # Refresh in-memory cache if middleware is running
    mw = _whitelist_middleware(request)
    if mw:
        await mw.refresh()

    return {"ok": True}


@router.post("/api/settings/whitelist/refresh")
async def refresh_whitelist(request: Request):
    """Reload whitelist middleware cache from DB."""
    mw = _whitelist_middleware(request)
    if not mw:
        return {"ok": False, "error": "whitelist middleware not running"}
    await mw.refresh()
    return {"ok": True}
