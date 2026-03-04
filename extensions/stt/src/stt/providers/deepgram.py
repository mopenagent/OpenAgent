"""Deepgram cloud STT provider using deepgram-sdk."""

from __future__ import annotations

import asyncio
import os
import time
from typing import Any

from openagent.observability import log_event
from openagent.observability.logging import get_logger
from openagent.observability.metrics import PROVIDER_CALL_SECONDS

from .base import STTProvider

logger = get_logger(__name__)


class DeepgramProvider(STTProvider):
    def __init__(self, *, api_key: str | None = None, model: str = "nova-3"):
        self.api_key = api_key or os.getenv("DEEPGRAM_API_KEY")
        self.model = model

    async def transcribe(self, audio_data: bytes, **kwargs) -> str:
        if not self.api_key:
            raise RuntimeError("DEEPGRAM_API_KEY is required for Deepgram provider.")
        language = kwargs.get("language", "en")
        punctuate = bool(kwargs.get("punctuate", True))
        smart_format = bool(kwargs.get("smart_format", True))
        timeout_s = float(kwargs.get("timeout_s", 20.0))
        retries = int(kwargs.get("retries", 1))
        start = time.perf_counter()
        status = "ok"
        last_exc: Exception | None = None

        def _transcribe_sync() -> str:
            from deepgram import DeepgramClient

            client = DeepgramClient(self.api_key)
            payload = {"buffer": audio_data}
            options = {
                "model": self.model,
                "punctuate": punctuate,
                "smart_format": smart_format,
                "language": language,
            }

            response = client.listen.prerecorded.v("1").transcribe_file(payload, options)
            return self._extract_transcript(response)

        try:
            for attempt in range(retries + 1):
                try:
                    result = await asyncio.wait_for(asyncio.to_thread(_transcribe_sync), timeout=timeout_s)
                    return result.strip()
                except Exception as exc:
                    status = "error"
                    last_exc = exc
                    log_event(
                        logger,
                        30,
                        "deepgram transcribe attempt failed",
                        component="provider.stt",
                        provider="deepgram",
                        operation="transcribe",
                        attempt=attempt + 1,
                        retries=retries,
                        error=str(exc),
                    )
                    if attempt >= retries:
                        raise
                    await asyncio.sleep(min(0.2 * (2**attempt), 1.0))

            if last_exc is not None:
                raise last_exc
            raise RuntimeError("deepgram transcribe failed")
        finally:
            elapsed = time.perf_counter() - start
            PROVIDER_CALL_SECONDS.labels(
                extension="stt",
                provider="deepgram",
                operation="transcribe",
                status=status,
            ).observe(elapsed)
            log_event(
                logger,
                20 if status == "ok" else 40,
                "deepgram transcribe complete",
                component="provider.stt",
                provider="deepgram",
                operation="transcribe",
                status=status,
                audio_bytes=len(audio_data),
                duration_ms=round(elapsed * 1000, 3),
            )

    @staticmethod
    def _extract_transcript(response: Any) -> str:
        if hasattr(response, "to_dict"):
            data = response.to_dict()
        elif isinstance(response, dict):
            data = response
        else:
            data = getattr(response, "__dict__", {})
        try:
            return (
                data["results"]["channels"][0]["alternatives"][0].get("transcript", "") or ""
            ).strip()
        except Exception:
            return ""
