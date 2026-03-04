"""MiniMax Speech 2.8 (t2a_v2) provider."""

from __future__ import annotations

import asyncio
import base64
import json
import os
from collections.abc import AsyncIterator
import time
from typing import Any

import aiohttp

from openagent.observability import log_event
from openagent.observability.logging import get_logger
from openagent.observability.metrics import PROVIDER_CALL_SECONDS

from .base import TTSProvider

logger = get_logger(__name__)


DEFAULT_MINIMAX_VOICE = "Calm_Woman"
DEFAULT_MINIMAX_API_URL = "https://api.minimax.chat/v1/t2a_v2"


class MiniMaxProvider(TTSProvider):
    def __init__(
        self,
        *,
        api_key: str | None = None,
        group_id: str | None = None,
        api_url: str | None = None,
        session: aiohttp.ClientSession | None = None,
    ):
        self.api_key = api_key or os.getenv("MINIMAX_API_KEY")
        self.group_id = group_id or os.getenv("MINIMAX_GROUP_ID")
        self.api_url = api_url or DEFAULT_MINIMAX_API_URL
        self._session = session

    async def generate(self, text: str, **kwargs) -> bytes:
        chunks = [chunk async for chunk in self.generate_stream(text, **kwargs)]
        return b"".join(chunks)

    async def generate_stream(self, text: str, **kwargs) -> AsyncIterator[bytes]:
        self._validate_credentials()
        voice = str(kwargs.get("voice_id", DEFAULT_MINIMAX_VOICE))
        speed = kwargs.get("speed", 1.0)
        vol = kwargs.get("vol", 1.0)
        stream = bool(kwargs.get("stream", True))
        timeout_s = float(kwargs.get("timeout_s", 20.0))
        retries = int(kwargs.get("retries", 1))
        start = time.perf_counter()
        status = "ok"
        payload = {
            "model": "speech-2.8-hd",
            "text": text,
            "voice_setting": {
                "voice_id": voice,
                "speed": speed,
                "vol": vol,
            },
            "audio_setting": {
                "sample_rate": 32000,
                "bitrate": 128000,
                "format": "mp3",
            },
            "stream": stream,
        }
        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
            "Group-Id": str(self.group_id),
        }
        own_session = self._session is None
        timeout = aiohttp.ClientTimeout(total=timeout_s)
        session = self._session or aiohttp.ClientSession(timeout=timeout)
        try:
            for attempt in range(retries + 1):
                try:
                    async with session.post(self.api_url, headers=headers, json=payload) as response:
                        response.raise_for_status()
                        if stream:
                            async for chunk in self._iter_streaming_audio(response):
                                yield chunk
                        else:
                            data = await response.json()
                            yield self._extract_audio_from_json(data)
                        return
                except Exception as exc:
                    status = "error"
                    log_event(
                        logger,
                        30,
                        "minimax generate attempt failed",
                        component="provider.tts",
                        provider="minimax",
                        operation="generate_stream",
                        attempt=attempt + 1,
                        retries=retries,
                        error=str(exc),
                    )
                    if attempt >= retries:
                        raise
                    await asyncio.sleep(min(0.2 * (2**attempt), 1.0))
        finally:
            elapsed = time.perf_counter() - start
            PROVIDER_CALL_SECONDS.labels(
                extension="tts",
                provider="minimax",
                operation="generate_stream",
                status=status,
            ).observe(elapsed)
            log_event(
                logger,
                20 if status == "ok" else 40,
                "minimax generate_stream complete",
                component="provider.tts",
                provider="minimax",
                operation="generate_stream",
                status=status,
                text_length=len(text),
                duration_ms=round(elapsed * 1000, 3),
            )
            if own_session:
                await session.close()

    async def _iter_streaming_audio(self, response: aiohttp.ClientResponse) -> AsyncIterator[bytes]:
        async for raw in response.content:
            line = raw.decode("utf-8", errors="ignore").strip()
            if not line:
                continue
            if line.startswith("data:"):
                line = line[len("data:") :].strip()
            if line == "[DONE]":
                break
            try:
                payload = json.loads(line)
            except json.JSONDecodeError:
                continue
            audio = self._extract_audio(payload)
            if audio:
                yield audio

    def _extract_audio_from_json(self, data: dict[str, Any]) -> bytes:
        audio = self._extract_audio(data)
        if not audio:
            raise RuntimeError("MiniMax response did not include audio payload.")
        return audio

    @staticmethod
    def _extract_audio(data: dict[str, Any]) -> bytes | None:
        candidates: list[str | None] = []
        candidates.append(data.get("audio"))
        result = data.get("data")
        if isinstance(result, dict):
            candidates.append(result.get("audio"))
            inner = result.get("audio_data")
            if isinstance(inner, dict):
                candidates.append(inner.get("data"))
        if isinstance(data.get("choices"), list):
            for choice in data["choices"]:
                if isinstance(choice, dict):
                    candidates.append(choice.get("audio"))
                    msg = choice.get("message")
                    if isinstance(msg, dict):
                        candidates.append(msg.get("audio"))
        for item in candidates:
            if not item:
                continue
            try:
                return base64.b64decode(item)
            except Exception:
                continue
        return None

    def _validate_credentials(self) -> None:
        if not self.api_key:
            raise RuntimeError("MINIMAX_API_KEY is required for MiniMax provider.")
        if not self.group_id:
            raise RuntimeError("MINIMAX_GROUP_ID is required for MiniMax provider.")
