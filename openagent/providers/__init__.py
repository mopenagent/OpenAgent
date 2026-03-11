"""openagent.providers — LLM provider factory."""

from __future__ import annotations

import logging

from openagent.observability import get_logger, log_event

from .anthropic import AnthropicProvider
from .base import LLMResponse, Message, Provider, ToolCall
from .cortex import CortexProvider
from .config import ProviderConfig, load_provider_config
from .openai import OpenAIProvider
from .openai_compat import OpenAICompatProvider

logger = get_logger(__name__)


def get_provider(
    cfg: ProviderConfig,
) -> AnthropicProvider | OpenAIProvider | OpenAICompatProvider:
    """Return the appropriate provider for the given config."""
    log_event(
        logger,
        logging.INFO,
        "initialising provider",
        component="providers",
        provider_kind=cfg.kind,
        model=cfg.model,
    )
    match cfg.kind:
        case "anthropic":
            return AnthropicProvider(cfg)
        case "openai":
            return OpenAIProvider(cfg)
        case _:  # "openai_compat" or unknown
            return OpenAICompatProvider(cfg)


__all__ = [
    "LLMResponse",
    "Message",
    "Provider",
    "ProviderConfig",
    "ToolCall",
    "load_provider_config",
    "get_provider",
    "OpenAICompatProvider",
    "AnthropicProvider",
    "CortexProvider",
    "OpenAIProvider",
]
