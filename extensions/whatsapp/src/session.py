"""Session and Neonize client setup for WhatsApp extension."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from time import time
from typing import Any, Callable, Protocol


class NeonizeUnavailableError(RuntimeError):
    pass


class GatewayClient(Protocol):
    def connect(self) -> None: ...

    def disconnect(self) -> None: ...

    def is_connected(self) -> bool: ...

    def send_message(self, chat_id: str, payload: dict[str, Any]) -> Any: ...


@dataclass(slots=True)
class SessionConfig:
    data_dir: Path
    account_id: str = "default"

    @property
    def account_dir(self) -> Path:
        return self.data_dir / "whatsapp" / self.account_id

    @property
    def session_db_path(self) -> Path:
        return self.account_dir / "session.db"

    @property
    def self_id_path(self) -> Path:
        return self.account_dir / "self_id.txt"


class SessionManager:
    def __init__(self, config: SessionConfig):
        self.config = config

    def ensure_storage(self) -> None:
        self.config.account_dir.mkdir(parents=True, exist_ok=True)
        self.config.session_db_path.touch(exist_ok=True)

    def is_linked(self) -> bool:
        path = self.config.session_db_path
        return path.exists() and path.stat().st_size > 0

    def auth_age_ms(self) -> int | None:
        path = self.config.session_db_path
        if not path.exists():
            return None
        modified_ms = path.stat().st_mtime_ns // 1_000_000
        now_ms = int(time() * 1000)
        return max(0, now_ms - int(modified_ms))

    def read_self_id(self) -> str | None:
        if not self.config.self_id_path.exists():
            return None
        value = self.config.self_id_path.read_text(encoding="utf-8").strip()
        return value or None

    def persist_self_id(self, value: str) -> None:
        self.config.self_id_path.write_text(value.strip(), encoding="utf-8")

    def create_client(
        self,
        *,
        on_qr: Callable[[str], None] | None = None,
        on_event: Callable[[Any], None] | None = None,
    ) -> GatewayClient:
        self.ensure_storage()
        try:
            neonize = __import__("neonize")
        except Exception as exc:  # pragma: no cover - environment dependent
            raise NeonizeUnavailableError(
                "neonize is not installed or could not be imported."
            ) from exc

        raw_client = None
        if hasattr(neonize, "ClientFactory"):
            raw_client = neonize.ClientFactory(str(self.config.session_db_path))
        elif hasattr(neonize, "Client"):
            raw_client = neonize.Client(str(self.config.session_db_path))
        if raw_client is None:
            raise NeonizeUnavailableError("Unsupported neonize client API.")

        if on_qr and hasattr(raw_client, "on_qr"):
            raw_client.on_qr(on_qr)
        if on_event and hasattr(raw_client, "on_event"):
            raw_client.on_event(on_event)

        return _NeonizeClientAdapter(raw_client)


class _NeonizeClientAdapter:
    def __init__(self, client: Any):
        self._client = client
        self._connected = False

    def connect(self) -> None:
        if hasattr(self._client, "connect"):
            self._client.connect()
        self._connected = True

    def disconnect(self) -> None:
        if hasattr(self._client, "disconnect"):
            self._client.disconnect()
        self._connected = False

    def is_connected(self) -> bool:
        if hasattr(self._client, "is_connected"):
            try:
                return bool(self._client.is_connected())
            except Exception:
                return self._connected
        return self._connected

    def send_message(self, chat_id: str, payload: dict[str, Any]) -> Any:
        if hasattr(self._client, "send_message"):
            return self._client.send_message(chat_id, payload)
        if hasattr(self._client, "send"):
            return self._client.send(chat_id, payload)
        raise RuntimeError("Neonize client does not provide send_message/send.")
