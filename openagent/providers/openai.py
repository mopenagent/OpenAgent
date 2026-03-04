"""OpenAI provider — OpenAI API (api.openai.com/v1)."""

from __future__ import annotations

from .config import ProviderConfig
from .openai_compat import OpenAICompatProvider

_OPENAI_BASE = "https://api.openai.com/v1"


class OpenAIProvider(OpenAICompatProvider):
    """OpenAI-specific provider; defaults base_url to api.openai.com."""

    def __init__(self, cfg: ProviderConfig) -> None:
        if not cfg.base_url:
            cfg = cfg.model_copy(update={"base_url": _OPENAI_BASE})
        super().__init__(cfg)
