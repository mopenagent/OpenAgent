"""Outbound payload builders for WhatsApp messaging."""

from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any

from session import GatewayClient


PCAP_MIME_TYPES = {
    ".pcap": "application/vnd.tcpdump.pcap",
    ".pcapng": "application/octet-stream",
}


class WhatsAppBuilders:
    def __init__(self, client: GatewayClient):
        self._client = client

    async def send_text(self, chat_id: str, text: str) -> Any:
        payload = {"type": "text", "text": text}
        return await asyncio.to_thread(self._client.send_message, chat_id, payload)

    async def send_image(self, chat_id: str, image_path: str, caption: str | None = None) -> Any:
        payload = {
            "type": "image",
            "path": image_path,
            "caption": caption or "",
        }
        return await asyncio.to_thread(self._client.send_message, chat_id, payload)

    async def send_document(
        self,
        chat_id: str,
        file_path: str,
        *,
        caption: str | None = None,
        mime_type: str | None = None,
        file_name: str | None = None,
    ) -> Any:
        path = Path(file_path)
        resolved_name = file_name or path.name
        resolved_mime = mime_type or PCAP_MIME_TYPES.get(path.suffix.lower(), "application/octet-stream")
        payload = {
            "type": "document",
            "path": str(path),
            "caption": caption or "",
            "mime_type": resolved_mime,
            "file_name": resolved_name,
        }
        return await asyncio.to_thread(self._client.send_message, chat_id, payload)
