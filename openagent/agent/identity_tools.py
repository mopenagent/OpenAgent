"""Identity linking tools — let users merge their cross-platform sessions.

Two tools are exposed to the agent:

identity.generate_link_pin
    Called when the user wants to link their account on this platform with
    another platform.  Generates a one-time 6-digit pin and stores it
    against the current session's user_key for 10 minutes.

    The user takes this pin to the *other* platform and says:
    "link my account, pin is 482931"

identity.redeem_link_pin
    Called on the *other* platform when the user provides the pin they got
    from the first platform.  Validates it, merges the two sessions (the
    generator's history absorbs the redeemer's), and returns the winning key.

After a successful redeem, both platforms resolve to the same user_key, so
all future messages share one conversation history regardless of platform.
"""

from __future__ import annotations

import json
import secrets
from datetime import datetime, timedelta
from typing import Any

from openagent.session.manager import SessionManager


def make_identity_tools(
    sessions: SessionManager,
) -> list[tuple[str, str, dict[str, Any], Any]]:
    """Return ``(name, description, params_schema, handler)`` tuples.

    Pass each tuple directly to ``ToolRegistry.register_native()``.
    """

    async def generate_link_pin(session_key: str, _args: dict[str, Any]) -> str:
        pin = f"{secrets.randbelow(1_000_000):06d}"
        expires_at = (datetime.now() + timedelta(minutes=10)).isoformat()
        await sessions.store_link_pin(session_key, pin, expires_at)
        return json.dumps({
            "pin": pin,
            "expires_in": "10 minutes",
            "instructions": (
                "Share this pin on the other platform and ask the assistant "
                "to redeem it using 'identity.redeem_link_pin'."
            ),
        })

    async def redeem_link_pin(session_key: str, args: dict[str, Any]) -> str:
        pin = str(args.get("pin", "")).strip()
        if not pin:
            return json.dumps({"error": "pin is required"})
        winning_key = await sessions.redeem_link_pin(session_key, pin)
        if winning_key is None:
            return json.dumps({
                "error": "invalid or expired pin — pins are valid for 10 minutes and can only be used once",
            })
        return json.dumps({
            "ok": True,
            "session_key": winning_key,
            "message": "Accounts linked. This conversation now shares history across all your connected platforms.",
        })

    return [
        (
            "identity.generate_link_pin",
            "Generate a one-time 6-digit pin to link this conversation with the same "
            "user on another platform. The pin expires in 10 minutes.",
            {"type": "object", "properties": {}},
            generate_link_pin,
        ),
        (
            "identity.redeem_link_pin",
            "Redeem a link pin generated on another platform to merge both conversation "
            "histories into one shared session. Ask the user for the 6-digit pin they received.",
            {
                "type": "object",
                "properties": {
                    "pin": {
                        "type": "string",
                        "description": "The 6-digit pin from the other platform.",
                    }
                },
                "required": ["pin"],
            },
            redeem_link_pin,
        ),
    ]
