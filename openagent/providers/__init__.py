"""openagent.providers — LLM provider factory."""

from __future__ import annotations

from .anthropic import AnthropicProvider
from .base import Message, Provider
from .config import ProviderConfig, load_provider_config
from .openai import OpenAIProvider
from .openai_compat import OpenAICompatProvider


def get_provider(
    cfg: ProviderConfig,
) -> OpenAICompatProvider | AnthropicProvider | OpenAIProvider:
    """Return a provider instance for the given config."""
    match cfg.kind:
        case "anthropic":
            return AnthropicProvider(cfg)
        case "openai":
            return OpenAIProvider(cfg)
        case _:  # "openai_compat" or unknown
            return OpenAICompatProvider(cfg)


__all__ = [
    "Message",
    "Provider",
    "ProviderConfig",
    "load_provider_config",
    "get_provider",
    "OpenAICompatProvider",
    "AnthropicProvider",
    "OpenAIProvider",
]
