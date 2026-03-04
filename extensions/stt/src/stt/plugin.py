"""STT extension entrypoint module."""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
import time
from typing import Any

from openagent.interfaces import BaseAsyncExtension
from openagent.observability import log_event
from openagent.observability.logging import get_logger
from openagent.observability.metrics import EXTENSION_OPERATION_SECONDS, PROVIDER_CALL_TOTAL

from .providers import DeepgramProvider, FasterWhisperProvider, STTProvider

logger = get_logger(__name__)


class STTExtension(BaseAsyncExtension):
    def __init__(self, *, config: dict[str, Any] | None = None):
        self._config = config or {}
        self._provider_name = str(
            self._config.get("provider") or os.getenv("OPENAGENT_STT_PROVIDER", "faster-whisper")
        ).lower()
        self._provider: STTProvider | None = None
        self._status: dict[str, Any] = {
            "running": False,
            "provider": self._provider_name,
        }

    async def initialize(self) -> None:
        self._provider = self._build_provider(self._provider_name)
        self._status["running"] = True
        log_event(
            logger,
            20,
            "stt extension initialized",
            component="extension.stt",
            operation="initialize",
            provider=self._provider_name,
        )

    async def shutdown(self) -> None:
        self._status["running"] = False
        log_event(
            logger,
            20,
            "stt extension stopped",
            component="extension.stt",
            operation="shutdown",
            provider=self._provider_name,
        )

    def get_status(self) -> dict[str, Any]:
        return dict(self._status)

    async def listen(
        self,
        *,
        stream=None,
        file: str | os.PathLike[str] | None = None,
        audio_data: bytes | None = None,
        chunk_bytes: int = 64000,
        **kwargs,
    ) -> str:
        provider = self._require_provider()
        start = time.perf_counter()
        status = "ok"

        try:
            if file is not None:
                file_path = Path(file)
                data = await asyncio.to_thread(file_path.read_bytes)
                text = await provider.transcribe(data, **kwargs)
                self._record_provider_call("listen", "ok", "none")
                return text

            if audio_data is not None:
                text = await provider.transcribe(audio_data, **kwargs)
                self._record_provider_call("listen", "ok", "none")
                return text

            if stream is not None:
                parts: list[str] = []
                buffer = bytearray()
                async for chunk in stream:
                    if not chunk:
                        continue
                    buffer.extend(chunk)
                    if len(buffer) >= chunk_bytes:
                        text = await provider.transcribe(bytes(buffer), **kwargs)
                        if text:
                            parts.append(text)
                        buffer.clear()
                if buffer:
                    text = await provider.transcribe(bytes(buffer), **kwargs)
                    if text:
                        parts.append(text)
                self._record_provider_call("listen", "ok", "none")
                return " ".join(parts).strip()

            raise ValueError("Provide one of: stream, file, or audio_data.")
        except Exception as exc:
            status = "error"
            self._record_provider_call("listen", "error", _error_type(exc))
            log_event(
                logger,
                40,
                "stt listen failed",
                component="extension.stt",
                operation="listen",
                provider=self._provider_name,
                error=str(exc),
            )
            raise
        finally:
            elapsed = time.perf_counter() - start
            EXTENSION_OPERATION_SECONDS.labels(
                extension="stt",
                provider=self._provider_name,
                operation="listen",
                status=status,
            ).observe(elapsed)

    async def listen_stream(self, stream, *, chunk_bytes: int = 64000, **kwargs):
        provider = self._require_provider()
        buffer = bytearray()
        start = time.perf_counter()
        status = "ok"
        chunk_count = 0
        bytes_total = 0

        try:
            async for chunk in stream:
                if not chunk:
                    continue
                bytes_total += len(chunk)
                buffer.extend(chunk)
                if len(buffer) >= chunk_bytes:
                    text = await provider.transcribe(bytes(buffer), **kwargs)
                    self._record_provider_call("listen_stream", "ok", "none")
                    chunk_count += 1
                    yield text
                    buffer.clear()
            if buffer:
                text = await provider.transcribe(bytes(buffer), **kwargs)
                self._record_provider_call("listen_stream", "ok", "none")
                chunk_count += 1
                yield text
        except Exception as exc:
            status = "error"
            self._record_provider_call("listen_stream", "error", _error_type(exc))
            raise
        finally:
            elapsed = time.perf_counter() - start
            EXTENSION_OPERATION_SECONDS.labels(
                extension="stt",
                provider=self._provider_name,
                operation="listen_stream",
                status=status,
            ).observe(elapsed)
            log_event(
                logger,
                20 if status == "ok" else 40,
                "stt listen_stream complete",
                component="extension.stt",
                operation="listen_stream",
                provider=self._provider_name,
                status=status,
                chunks=chunk_count,
                audio_bytes=bytes_total,
                duration_ms=round(elapsed * 1000, 3),
            )

    def _require_provider(self) -> STTProvider:
        if self._provider is None:
            raise RuntimeError("STTExtension is not initialized.")
        return self._provider

    def _build_provider(self, provider_name: str) -> STTProvider:
        if provider_name in {"faster-whisper", "whisper", "local"}:
            model_size = str(self._config.get("whisper_model", "small"))
            return FasterWhisperProvider(model_size=model_size)
        if provider_name == "deepgram":
            return DeepgramProvider()
        raise ValueError(f"Unsupported STT provider '{provider_name}'.")

    def _record_provider_call(self, operation: str, status: str, error_type: str) -> None:
        PROVIDER_CALL_TOTAL.labels(
            extension="stt",
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
