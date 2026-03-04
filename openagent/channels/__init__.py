"""Channel contracts and adapters."""

from .adapter import (
    ChannelAdapter,
    DiscordChannelAdapter,
    SlackChannelAdapter,
    TelegramChannelAdapter,
)
from .discord import DiscordServiceChannel
from .manager import ChannelManager
from .telegram import TelegramServiceChannel
from .whatsapp import WhatsAppTransport

__all__ = [
    # Push-model adapters (production)
    "ChannelAdapter",
    "ChannelManager",
    "DiscordChannelAdapter",
    "TelegramChannelAdapter",
    "SlackChannelAdapter",
    # Legacy pull-model clients (tests / standalone use)
    "DiscordServiceChannel",
    "TelegramServiceChannel",
    "WhatsAppTransport",
]
