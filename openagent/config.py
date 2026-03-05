"""Full OpenAgent configuration schema and loader.

Sections
--------
provider:   LLM backend (existing, unchanged)
agents:     Agent definitions (name, system_prompt, limits)
session:    Session storage settings
channels:   Per-channel credentials (injected into Go service env vars)
tools:      Tool policy (allowed filesystem paths, shell commands)

Loading order
-------------
1. config/openagent.yaml  (defaults)
2. Environment variables  (OPENAGENT_* prefix, always win)

Backward compat
---------------
``load_provider_config()`` in ``openagent/providers/config.py`` still works
unchanged.  New code should call ``load_config()`` instead and access
``cfg.provider``.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Literal, Optional

import yaml
from pydantic import BaseModel, Field, field_validator

from openagent.providers.config import ProviderConfig


# ---------------------------------------------------------------------------
# Agent
# ---------------------------------------------------------------------------


class AgentConfig(BaseModel):
    name: str = "default"
    system_prompt: str = (
        "You are a helpful assistant. "
        "Use tools only when necessary. "
        "Be concise."
    )
    max_iterations: int = 40
    max_tool_output: int = 500  # chars before truncation


# ---------------------------------------------------------------------------
# Session
# ---------------------------------------------------------------------------


class SessionConfig(BaseModel):
    summarise_after: int = 40
    backend: Literal["sqlite"] = "sqlite"
    db_path: str = "data/sessions.db"


# ---------------------------------------------------------------------------
# Channels (credentials only — socket paths are in service.json)
# ---------------------------------------------------------------------------


class DiscordChannelConfig(BaseModel):
    token: str = ""
    guild_ids: list[int] = Field(default_factory=list)


class TelegramChannelConfig(BaseModel):
    app_id: int = 0
    app_hash: str = ""
    bot_token: str = ""


class SlackChannelConfig(BaseModel):
    bot_token: str = ""
    app_token: str = ""


class WhatsAppChannelConfig(BaseModel):
    phone_number: str = ""
    data_dir: str = "data/whatsapp"


class ChannelsConfig(BaseModel):
    discord: Optional[DiscordChannelConfig] = None
    telegram: Optional[TelegramChannelConfig] = None
    slack: Optional[SlackChannelConfig] = None
    whatsapp: Optional[WhatsAppChannelConfig] = None


# ---------------------------------------------------------------------------
# Tools policy
# ---------------------------------------------------------------------------


class FilesystemToolConfig(BaseModel):
    allowed_paths: list[str] = Field(default_factory=list)


class ShellToolConfig(BaseModel):
    # Empty list disables shell tool entirely.
    allowed_commands: list[str] = Field(default_factory=list)


class ToolsConfig(BaseModel):
    filesystem: Optional[FilesystemToolConfig] = None
    shell: Optional[ShellToolConfig] = None


# ---------------------------------------------------------------------------
# Root
# ---------------------------------------------------------------------------


class OpenAgentConfig(BaseModel):
    provider: ProviderConfig = Field(default_factory=ProviderConfig)
    agents: list[AgentConfig] = Field(default_factory=lambda: [AgentConfig()])
    session: SessionConfig = Field(default_factory=SessionConfig)
    channels: ChannelsConfig = Field(default_factory=ChannelsConfig)
    tools: ToolsConfig = Field(default_factory=ToolsConfig)

    @field_validator("tools", mode="before")
    @classmethod
    def _coerce_tools_none(cls, v: object) -> object:
        return v if v is not None else {}

    @property
    def default_agent(self) -> AgentConfig:
        return self.agents[0] if self.agents else AgentConfig()


# ---------------------------------------------------------------------------
# Environment variable overrides
# ---------------------------------------------------------------------------

# (yaml_path_tuple, env_var_names)  — first non-empty env var wins
_ENV_OVERRIDES: list[tuple[tuple[str, ...], list[str]]] = [
    # Channels — Discord
    (("channels", "discord", "token"),
     ["DISCORD_TOKEN", "OPENAGENT_DISCORD_TOKEN"]),
    # Channels — Telegram
    (("channels", "telegram", "app_id"),
     ["TELEGRAM_APP_ID", "OPENAGENT_TELEGRAM_APP_ID"]),
    (("channels", "telegram", "app_hash"),
     ["TELEGRAM_APP_HASH", "OPENAGENT_TELEGRAM_APP_HASH"]),
    (("channels", "telegram", "bot_token"),
     ["TELEGRAM_BOT_TOKEN", "OPENAGENT_TELEGRAM_BOT_TOKEN"]),
    # Channels — Slack
    (("channels", "slack", "bot_token"),
     ["SLACK_BOT_TOKEN", "OPENAGENT_SLACK_BOT_TOKEN"]),
    (("channels", "slack", "app_token"),
     ["SLACK_APP_TOKEN", "OPENAGENT_SLACK_APP_TOKEN"]),
    # Channels — WhatsApp
    (("channels", "whatsapp", "phone_number"),
     ["WHATSAPP_PHONE", "OPENAGENT_WHATSAPP_PHONE"]),
]


def _apply_env_overrides(data: dict) -> dict:
    """Overlay environment variables onto the raw config dict (mutates in place)."""
    for path, env_vars in _ENV_OVERRIDES:
        value = next((os.environ[v] for v in env_vars if v in os.environ), None)
        if value is None:
            continue
        # Ensure intermediate dicts exist
        node = data
        for key in path[:-1]:
            node = node.setdefault(key, {})
        node[path[-1]] = value
    return data


# ---------------------------------------------------------------------------
# Loader
# ---------------------------------------------------------------------------


def load_config(yaml_path: Path | None = None) -> OpenAgentConfig:
    """Load the full OpenAgent configuration.

    Priority: env vars > YAML file > schema defaults.
    """
    data: dict = {}

    if yaml_path and yaml_path.exists():
        raw = yaml.safe_load(yaml_path.read_text()) or {}
        data = dict(raw)

    # Provider env vars (existing behaviour from providers/config.py)
    from openagent.providers.config import _ENV_MAP  # noqa: PLC0415

    provider_data = dict(data.get("provider", {}))
    for field, env_var in _ENV_MAP.items():
        val = os.environ.get(env_var)
        if val is not None:
            provider_data[field] = val
    if provider_data:
        data["provider"] = provider_data

    _apply_env_overrides(data)
    return OpenAgentConfig.model_validate(data)


def build_service_env_extras(cfg: OpenAgentConfig) -> dict[str, dict[str, str]]:
    """Build per-service env var maps to inject when launching Go services.

    ServiceManager merges these into the subprocess env on each launch.
    Keys are service names (matching service.json ``name`` fields).
    """
    extras: dict[str, dict[str, str]] = {}

    discord = cfg.channels.discord
    if discord and discord.token:
        extras["discord"] = {"DISCORD_BOT_TOKEN": discord.token}

    telegram = cfg.channels.telegram
    if telegram:
        tg: dict[str, str] = {}
        if telegram.app_id:
            tg["TELEGRAM_APP_ID"] = str(telegram.app_id)
        if telegram.app_hash:
            tg["TELEGRAM_APP_HASH"] = telegram.app_hash
        if telegram.bot_token:
            tg["TELEGRAM_BOT_TOKEN"] = telegram.bot_token
        if tg:
            extras["telegram"] = tg

    slack = cfg.channels.slack
    if slack:
        sl: dict[str, str] = {}
        if slack.bot_token:
            sl["SLACK_BOT_TOKEN"] = slack.bot_token
        if slack.app_token:
            sl["SLACK_APP_TOKEN"] = slack.app_token
        if sl:
            extras["slack"] = sl

    return extras
