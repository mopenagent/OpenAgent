"""platform contracts and adapters."""

from .adapter import (
    PlatformAdapter,
    DiscordPlatformAdapter,
    SlackPlatformAdapter,
    TelegramPlatformAdapter,
)
from .discord import DiscordServicePlatform
from .telegram import TelegramServicePlatform
from .whatsapp import WhatsAppTransport

__all__ = [
    # Push-model adapters (production)
    "PlatformAdapter",
    "DiscordPlatformAdapter",
    "TelegramPlatformAdapter",
    "SlackPlatformAdapter",
    # Legacy pull-model clients (tests / standalone use)
    "DiscordServicePlatform",
    "TelegramServicePlatform",
    "WhatsAppTransport",
]
