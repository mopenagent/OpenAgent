"""TTS extension entrypoint module."""

from __future__ import annotations

import os
from collections.abc import AsyncIterator
import time
from typing import Any

from openagent.interfaces import BaseAsyncExtension
from openagent.observability import log_event
from openagent.observability.logging import get_logger
from openagent.observability.metrics import EXTENSION_OPERATION_SECONDS, PROVIDER_CALL_TOTAL

from .providers import EdgeProvider, MiniMaxProvider, TTSProvider

logger = get_logger(__name__)


class TTSExtension(BaseAsyncExtension):
    def __init__(self, *, config: dict[str, Any] | None = None):
        self._config = config or {}
        self._provider_name = str(
            self._config.get("provider")
            or os.getenv("OPENAGENT_TTS_PROVIDER", "edge")
        ).lower()
        self._provider: TTSProvider | None = None
        self._status: dict[str, Any] = {"running": False, "provider": self._provider_name}

    async def initialize(self) -> None:
        self._provider = self._build_provider(self._provider_name)
        self._status["running"] = True
        log_event(
            logger,
            20,
            "tts extension initialized",
            component="extension.tts",
            operation="initialize",
            provider=self._provider_name,
        )

    async def shutdown(self) -> None:
        self._status["running"] = False
        log_event(
            logger,
            20,
            "tts extension stopped",
            component="extension.tts",
            operation="shutdown",
            provider=self._provider_name,
        )

    def get_status(self) -> dict[str, Any]:
        return dict(self._status)

    async def speak(self, text: str, **kwargs) -> bytes:
        provider = self._require_provider()
        start = time.perf_counter()
        status = "ok"
        try:
            audio = await provider.generate(text, **kwargs)
            self._record_provider_call("speak", "ok", "none")
            return audio
        except Exception as exc:
            status = "error"
            self._record_provider_call("speak", "error", _error_type(exc))
            log_event(
                logger,
                40,
                "tts speak failed",
                component="extension.tts",
                operation="speak",
                provider=self._provider_name,
                error=str(exc),
            )
            raise
        finally:
            elapsed = time.perf_counter() - start
            EXTENSION_OPERATION_SECONDS.labels(
                extension="tts",
                provider=self._provider_name,
                operation="speak",
                status=status,
            ).observe(elapsed)

    async def speak_stream(self, text: str, **kwargs) -> AsyncIterator[bytes]:
        provider = self._require_provider()
        start = time.perf_counter()
        status = "ok"
        chunk_count = 0
        byte_total = 0

        try:
            async for chunk in provider.generate_stream(text, **kwargs):
                chunk_count += 1
                byte_total += len(chunk)
                self._record_provider_call("speak_stream", "ok", "none")
                yield chunk
        except Exception as exc:
            status = "error"
            self._record_provider_call("speak_stream", "error", _error_type(exc))
            raise
        finally:
            elapsed = time.perf_counter() - start
            EXTENSION_OPERATION_SECONDS.labels(
                extension="tts",
                provider=self._provider_name,
                operation="speak_stream",
                status=status,
            ).observe(elapsed)
            log_event(
                logger,
                20 if status == "ok" else 40,
                "tts speak_stream complete",
                component="extension.tts",
                operation="speak_stream",
                provider=self._provider_name,
                status=status,
                chunks=chunk_count,
                audio_bytes=byte_total,
                text_length=len(text),
                duration_ms=round(elapsed * 1000, 3),
            )

    def _require_provider(self) -> TTSProvider:
        if self._provider is None:
            raise RuntimeError("TTSExtension is not initialized.")
        return self._provider

    def _build_provider(self, provider_name: str) -> TTSProvider:
        if provider_name == "edge":
            return EdgeProvider()
        if provider_name == "minimax":
            return MiniMaxProvider()
        raise ValueError(f"Unsupported TTS provider '{provider_name}'.")

    def _record_provider_call(self, operation: str, status: str, error_type: str) -> None:
        PROVIDER_CALL_TOTAL.labels(
            extension="tts",
            provider=self._provider_name,
            operation=operation,
            status=status,
            error_type=error_type,
        ).inc()


def _error_type(exc: Exception) -> str:
    if isinstance(exc, TimeoutError):
        return "timeout"
    text = str(exc).lower()
    if "api_key" in text or "token" in text or "auth" in text:
        return "auth"
    return "runtime"
