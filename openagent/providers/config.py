"""Provider configuration — Pydantic model with YAML + env-var loading."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Literal

import yaml
from pydantic import BaseModel, Field

ProviderKind = Literal["openai_compat", "anthropic", "openai"]


class ProviderConfig(BaseModel):
    kind: ProviderKind = "openai_compat"
    base_url: str = "http://100.74.210.70:1234/v1"
    api_key: str = ""
    model: str = ""
    timeout: float = 60.0
    max_tokens: int = 2048


# Env-var overrides (OPENAGENT_ prefix)
_ENV_MAP: dict[str, str] = {
    "kind":     "OPENAGENT_PROVIDER_KIND",
    "base_url": "OPENAGENT_LLM_BASE_URL",
    "api_key":  "OPENAGENT_API_KEY",
    "model":    "OPENAGENT_MODEL",
    "timeout":  "OPENAGENT_LLM_TIMEOUT",
}


def load_provider_config(yaml_path: Path | None = None) -> ProviderConfig:
    """Load config from YAML file (provider: section), then overlay env vars."""
    data: dict = {}

    if yaml_path and yaml_path.exists():
        raw = yaml.safe_load(yaml_path.read_text()) or {}
        data = raw.get("provider", {})

    for field, env_var in _ENV_MAP.items():
        val = os.environ.get(env_var)
        if val is not None:
            data[field] = val

    return ProviderConfig(**data)
