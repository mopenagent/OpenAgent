"""Settings route — GET /settings (Provider + Connector + Guard tabs)."""

from __future__ import annotations

import io
import base64
import json

import httpx
from fastapi import APIRouter, Request
from fastapi.responses import JSONResponse
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
# Connectors API — state read from openagent.toml; runtime toggle in-memory
# ---------------------------------------------------------------------------

CONNECTOR_INFO = {
    "discord":  {"description": "Discord bot for servers and DMs. Configure token in config/openagent.toml."},
    "slack":    {"description": "Slack workspace bot. Requires bot token and app token."},
    "telegram": {"description": "Telegram bot or MTProto client. Configure in config/openagent.toml."},
    "whatsapp": {"description": "WhatsApp via whatsmeow (Go). Link phone via QR code below."},
}


def _api(request: Request) -> httpx.AsyncClient | None:
    return getattr(request.app.state, "api_client", None)


@router.get("/api/settings/connectors")
async def list_connectors(request: Request):
    """Return connector list with enabled state from openagent.toml and running status from /health."""
    cfg = getattr(request.app.state, "config", None)
    platforms = getattr(cfg, "platforms", None) if cfg else None

    # Runtime override map (toggled via PATCH, lives in memory, resets on restart)
    enabled_map: dict[str, bool] = getattr(request.app.state, "connectors_enabled", {})

    # Running status from Rust /health
    running: dict[str, bool] = {}
    api = _api(request)
    if api:
        try:
            resp = await api.get("/health", timeout=2.0)
            if resp.is_success:
                health = resp.json()
                for svc in health.get("services", []):
                    running[svc["name"]] = svc.get("status") == "running"
        except Exception:
            pass

    connectors = []
    for name, info in CONNECTOR_INFO.items():
        # Enabled: in-memory override first, then toml
        if name in enabled_map:
            enabled = enabled_map[name]
        elif platforms:
            plat = getattr(platforms, name, None)
            enabled = bool(getattr(plat, "enabled", False)) if plat else False
        else:
            enabled = False

        connectors.append({
            "name": name,
            "description": info["description"],
            "enabled": enabled,
            "running": running.get(name, False),
        })
    return {"connectors": connectors}


@router.patch("/api/settings/connectors/{name}")
async def patch_connector(request: Request, name: str):
    """Enable or disable a connector at runtime (in-memory; edit openagent.toml for persistence)."""
    body = await request.json()
    enabled = body.get("enabled")
    if enabled is None:
        return {"ok": False, "error": "enabled required"}
    if name not in CONNECTOR_INFO:
        return {"ok": False, "error": f"unknown connector: {name}"}

    # Update in-memory map
    enabled_map = getattr(request.app.state, "connectors_enabled", {})
    enabled_map[name] = bool(enabled)
    request.app.state.connectors_enabled = enabled_map

    # Signal Rust binary (best-effort)
    api = _api(request)
    if api:
        tool = "service.start" if enabled else "service.stop"
        try:
            await api.post(f"/tool/{tool}", content=json.dumps({"name": name}),
                           headers={"Content-Type": "application/json"})
        except Exception:
            pass

    return {"ok": True, "action": "started" if enabled else "stopped",
            "note": "In-memory only — edit config/openagent.toml for persistence"}


# ---------------------------------------------------------------------------
# WhatsApp QR
# ---------------------------------------------------------------------------

@router.get("/api/settings/whatsapp/qr")
async def whatsapp_qr(request: Request):
    """Return WhatsApp QR code as data URL for scanning."""
    qr_text: str | None = None
    connected = False
    status = "unavailable"

    api = _api(request)
    if api:
        try:
            resp = await api.post("/tool/whatsapp.qr", content="{}",
                                  headers={"Content-Type": "application/json"}, timeout=5.0)
            if resp.is_success:
                data = resp.json()
                qr_text = data.get("qr_text") or None
                connected = data.get("connected", False)
                status = "connected" if connected else ("pending" if qr_text else "waiting")
        except Exception:
            pass

    if not qr_text:
        if status == "unavailable":
            msg = "WhatsApp service not available. Ensure the service is built and running."
        elif connected:
            msg = "WhatsApp is already connected — no QR needed."
        else:
            msg = "Waiting for QR code… Click 'Refresh' to retry."
        return {"qr": None, "connected": connected, "status": status, "message": msg}

    try:
        import qrcode
        img = qrcode.make(qr_text)
        buf = io.BytesIO()
        img.save(buf, format="PNG")
        buf.seek(0)
        b64 = base64.b64encode(buf.read()).decode("ascii")
        return {
            "qr": f"data:image/png;base64,{b64}",
            "connected": connected,
            "status": status,
            "message": "Scan with WhatsApp: Settings > Linked Devices > Link a Device",
        }
    except Exception as e:
        return {"qr": None, "connected": connected, "status": "error",
                "message": f"QR generation failed: {e}"}


# ---------------------------------------------------------------------------
# Whitelist API — /api/settings/whitelist  (what the UI calls)
# Bridges to the guard service: guard.list / guard.allow / guard.remove.
# ---------------------------------------------------------------------------

@router.get("/api/settings/whitelist")
async def list_whitelist(request: Request):
    """Return all allowed contacts (status=allowed) as whitelist entries."""
    api = _api(request)
    if not api:
        return {"entries": [], "count": 0}
    try:
        data = await _guard_call(api, "guard.list", {})
        entries = [
            {
                "platform":   e["platform"],
                "channel_id": e["channel_id"],
                "label":      e.get("name", ""),
                "added_by":   e.get("note", ""),
                "added_at":   e.get("first_seen", ""),
            }
            for e in data.get("entries", [])
            if e.get("status") == "allowed"
        ]
        return {"entries": entries, "count": len(entries)}
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=503)


@router.post("/api/settings/whitelist")
async def add_whitelist(request: Request):
    """Allow a contact (add to guard table as status=allowed)."""
    body = await request.json()
    platform   = body.get("platform", "").strip()
    channel_id = body.get("channel_id", "").strip()
    label      = body.get("label", "").strip()
    added_by   = body.get("added_by", "").strip()
    if not platform or not channel_id:
        return JSONResponse({"error": "platform and channel_id required"}, status_code=400)
    api = _api(request)
    if not api:
        return JSONResponse({"error": "api unavailable"}, status_code=503)
    try:
        return await _guard_call(api, "guard.allow", {
            "platform": platform, "channel_id": channel_id,
            "name": label, "note": added_by,
        })
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=503)


@router.delete("/api/settings/whitelist/{platform}/{channel_id:path}")
async def remove_whitelist(request: Request, platform: str, channel_id: str):
    """Remove a contact from the whitelist."""
    api = _api(request)
    if not api:
        return JSONResponse({"error": "api unavailable"}, status_code=503)
    try:
        return await _guard_call(api, "guard.remove", {"platform": platform, "channel_id": channel_id})
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=503)


@router.get("/api/settings/whitelist/seen")
async def list_seen_senders(request: Request):
    """Return unknown/blocked senders (contacts seen but not yet whitelisted)."""
    api = _api(request)
    if not api:
        return {"senders": []}
    try:
        data = await _guard_call(api, "guard.list", {})
        senders = [
            {
                "platform":     e["platform"],
                "channel_id":   e["channel_id"],
                "message_count": e.get("hit_count", 0),
                "last_seen":    e.get("last_seen", ""),
            }
            for e in data.get("entries", [])
            if e.get("status") in ("unknown", "blocked")
        ]
        return {"senders": senders}
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=503)


# ---------------------------------------------------------------------------
# Guard API — contacts (allowed/blocked/unknown) via Rust guard service
# ---------------------------------------------------------------------------

async def _guard_call(api: httpx.AsyncClient, tool: str, params: dict) -> dict:
    """Call a guard tool on the Rust openagent API."""
    resp = await api.post(f"/tool/{tool}", content=json.dumps(params),
                          headers={"Content-Type": "application/json"}, timeout=5.0)
    resp.raise_for_status()
    return resp.json()


@router.get("/api/settings/guard")
async def list_guard(request: Request):
    """Return all contacts in the guard table (allowed + blocked + unknown)."""
    api = _api(request)
    if not api:
        return {"entries": [], "count": 0}
    try:
        data = await _guard_call(api, "guard.list", {})
        return data
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=503)


@router.post("/api/settings/guard/allow")
async def guard_allow(request: Request):
    """Allow a contact (add to guard table as status=allowed)."""
    body = await request.json()
    platform = body.get("platform", "").strip()
    channel_id = body.get("channel_id", "").strip()
    name = body.get("name", "").strip()
    if not platform or not channel_id:
        return JSONResponse({"error": "platform and channel_id required"}, status_code=400)
    api = _api(request)
    if not api:
        return JSONResponse({"error": "api unavailable"}, status_code=503)
    try:
        return await _guard_call(api, "guard.allow", {"platform": platform, "channel_id": channel_id, "name": name})
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=503)


@router.post("/api/settings/guard/block")
async def guard_block(request: Request):
    """Block a contact."""
    body = await request.json()
    platform = body.get("platform", "").strip()
    channel_id = body.get("channel_id", "").strip()
    note = body.get("note", "").strip()
    if not platform or not channel_id:
        return JSONResponse({"error": "platform and channel_id required"}, status_code=400)
    api = _api(request)
    if not api:
        return JSONResponse({"error": "api unavailable"}, status_code=503)
    try:
        return await _guard_call(api, "guard.block", {"platform": platform, "channel_id": channel_id, "note": note})
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=503)


@router.patch("/api/settings/guard/{platform}/{channel_id:path}/name")
async def guard_rename(request: Request, platform: str, channel_id: str):
    """Rename a contact in the guard table."""
    body = await request.json()
    name = body.get("name", "").strip()
    if not name:
        return JSONResponse({"error": "name required"}, status_code=400)
    api = _api(request)
    if not api:
        return JSONResponse({"error": "api unavailable"}, status_code=503)
    try:
        return await _guard_call(api, "guard.name", {"platform": platform, "channel_id": channel_id, "name": name})
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=503)


@router.delete("/api/settings/guard/{platform}/{channel_id:path}")
async def guard_remove(request: Request, platform: str, channel_id: str):
    """Remove a contact from the guard table."""
    api = _api(request)
    if not api:
        return JSONResponse({"error": "api unavailable"}, status_code=503)
    try:
        return await _guard_call(api, "guard.remove", {"platform": platform, "channel_id": channel_id})
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=503)
