"""Full OpenAgent configuration schema and loader.

Sections
--------
provider:   LLM backend (existing, unchanged)
agents:     Agent definitions (name, system_prompt, limits)
session:    Session storage settings
platforms:   Per-platform credentials (injected into Go service env vars)
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
# platforms (credentials only — socket paths are in service.json)
# ---------------------------------------------------------------------------


class DiscordPlatformConfig(BaseModel):
    token: str = ""
    guild_ids: list[int] = Field(default_factory=list)


class TelegramPlatformConfig(BaseModel):
    app_id: int = 0
    app_hash: str = ""
    bot_token: str = ""


class SlackPlatformConfig(BaseModel):
    bot_token: str = ""
    app_token: str = ""


class WhatsAppPlatformConfig(BaseModel):
    phone_number: str = ""
    data_dir: str = "data"  # parent dir for whatsapp.db


class PlatformsConfig(BaseModel):
    discord: Optional[DiscordPlatformConfig] = None
    telegram: Optional[TelegramPlatformConfig] = None
    slack: Optional[SlackPlatformConfig] = None
    whatsapp: Optional[WhatsAppPlatformConfig] = None


# ---------------------------------------------------------------------------
# Tools policy
# ---------------------------------------------------------------------------


class FilesystemToolConfig(BaseModel):
    allowed_paths: list[str] = Field(default_factory=list)


class SandboxToolConfig(BaseModel):
    """microsandbox server connection settings.

    MSB_SERVER_URL and MSB_API_KEY are the preferred env vars.
    These fields act as fallback config-file values.
    """
    server_url: str = "http://127.0.0.1:5555"
    api_key: str = ""      # required at runtime; set via MSB_API_KEY
    memory_mb: int = 512   # VM memory per sandbox call


class ToolsConfig(BaseModel):
    filesystem: Optional[FilesystemToolConfig] = None
    sandbox: Optional[SandboxToolConfig] = None


# ---------------------------------------------------------------------------
# Whitelist
# ---------------------------------------------------------------------------


class WhitelistConfig(BaseModel):
    enabled: bool = False


# ---------------------------------------------------------------------------
# STT
# ---------------------------------------------------------------------------


class STTConfig(BaseModel):
    provider: str = "faster-whisper"
    whisper_model: str = "small"


# ---------------------------------------------------------------------------
# TTS
# ---------------------------------------------------------------------------


class TTSConfig(BaseModel):
    provider: str = "edge"           # edge | minimax
    voice: str = "en-US-AriaNeural"  # Edge voice name or MiniMax voice_id
    speed: str = "+0%"               # Edge: "+10%" etc; MiniMax: float string e.g. "1.2"
    volume: str = "+0%"              # Edge: "+10%" etc; MiniMax: float string e.g. "1.0"
    # MiniMax-only credentials (prefer env vars MINIMAX_API_KEY / MINIMAX_GROUP_ID)
    api_key: str = ""
    group_id: str = ""


# ---------------------------------------------------------------------------
# Root
# ---------------------------------------------------------------------------


class OpenAgentConfig(BaseModel):
    provider: ProviderConfig = Field(default_factory=ProviderConfig)
    agents: list[AgentConfig] = Field(default_factory=lambda: [AgentConfig()])
    session: SessionConfig = Field(default_factory=SessionConfig)
    platforms: PlatformsConfig = Field(default_factory=PlatformsConfig)
    tools: ToolsConfig = Field(default_factory=ToolsConfig)
    stt: STTConfig = Field(default_factory=STTConfig)
    tts: TTSConfig = Field(default_factory=TTSConfig)
    whitelist: WhitelistConfig = Field(default_factory=WhitelistConfig)

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
    # platforms — Discord
    (("platforms", "discord", "token"),
     ["DISCORD_TOKEN", "OPENAGENT_DISCORD_TOKEN"]),
    # platforms — Telegram
    (("platforms", "telegram", "app_id"),
     ["TELEGRAM_APP_ID", "OPENAGENT_TELEGRAM_APP_ID"]),
    (("platforms", "telegram", "app_hash"),
     ["TELEGRAM_APP_HASH", "OPENAGENT_TELEGRAM_APP_HASH"]),
    (("platforms", "telegram", "bot_token"),
     ["TELEGRAM_BOT_TOKEN", "OPENAGENT_TELEGRAM_BOT_TOKEN"]),
    # platforms — Slack
    (("platforms", "slack", "bot_token"),
     ["SLACK_BOT_TOKEN", "OPENAGENT_SLACK_BOT_TOKEN"]),
    (("platforms", "slack", "app_token"),
     ["SLACK_APP_TOKEN", "OPENAGENT_SLACK_APP_TOKEN"]),
    # platforms — WhatsApp
    (("platforms", "whatsapp", "phone_number"),
     ["WHATSAPP_PHONE", "OPENAGENT_WHATSAPP_PHONE"]),
    # tts — MiniMax credentials
    (("tts", "api_key"),   ["MINIMAX_API_KEY", "OPENAGENT_MINIMAX_API_KEY"]),
    (("tts", "group_id"),  ["MINIMAX_GROUP_ID", "OPENAGENT_MINIMAX_GROUP_ID"]),
    # tools — sandbox (microsandbox)
    (("tools", "sandbox", "server_url"),
     ["MSB_SERVER_URL", "OPENAGENT_MSB_SERVER_URL"]),
    (("tools", "sandbox", "api_key"),
     ["MSB_API_KEY", "OPENAGENT_MSB_API_KEY"]),
    (("tools", "sandbox", "memory_mb"),
     ["MSB_MEMORY_MB", "OPENAGENT_MSB_MEMORY_MB"]),
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


def build_service_env_extras(cfg: OpenAgentConfig, root: Path) -> dict[str, dict[str, str]]:
    """Build per-service env var maps to inject when launching Go services.

    ServiceManager merges these into the subprocess env on each launch.
    Keys are service names (matching service.json ``name`` fields).
    Relative paths (e.g. data_dir) are resolved against root so the subprocess
    gets absolute paths regardless of its working directory.
    """
    extras: dict[str, dict[str, str]] = {}

    discord = cfg.platforms.discord
    if discord and discord.token:
        extras["discord"] = {"DISCORD_BOT_TOKEN": discord.token}

    # channels omnibus — inject all platform credentials so channels.toml
    # ${VAR} interpolation works without the user setting shell env vars manually
    channels: dict[str, str] = {}
    if discord and discord.token:
        channels["DISCORD_BOT_TOKEN"] = discord.token
    telegram = cfg.platforms.telegram
    if telegram:
        if telegram.bot_token:
            channels["TELEGRAM_BOT_TOKEN"] = telegram.bot_token
        if telegram.app_id:
            channels["TELEGRAM_APP_ID"] = str(telegram.app_id)
        if telegram.app_hash:
            channels["TELEGRAM_APP_HASH"] = telegram.app_hash
    slack = cfg.platforms.slack
    if slack:
        if slack.bot_token:
            channels["SLACK_BOT_TOKEN"] = slack.bot_token
        if slack.app_token:
            channels["SLACK_APP_TOKEN"] = slack.app_token
    if channels:
        extras["channels"] = channels

    telegram = cfg.platforms.telegram
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

    slack = cfg.platforms.slack
    if slack:
        sl: dict[str, str] = {}
        if slack.bot_token:
            sl["SLACK_BOT_TOKEN"] = slack.bot_token
        if slack.app_token:
            sl["SLACK_APP_TOKEN"] = slack.app_token
        if sl:
            extras["slack"] = sl

    whatsapp = cfg.platforms.whatsapp
    if whatsapp and whatsapp.data_dir:
        data_dir = Path(whatsapp.data_dir)
        if not data_dir.is_absolute():
            data_dir = (root / data_dir).resolve()
        extras["whatsapp"] = {
            "WHATSAPP_DATA_DIR": str(data_dir),
        }

    sandbox = cfg.tools.sandbox
    if sandbox:
        sb: dict[str, str] = {}
        if sandbox.server_url:
            sb["MSB_SERVER_URL"] = sandbox.server_url
        if sandbox.api_key:
            sb["MSB_API_KEY"] = sandbox.api_key
        if sandbox.memory_mb:
            sb["MSB_MEMORY_MB"] = str(sandbox.memory_mb)
        if sb:
            extras["sandbox"] = sb

    return extras
