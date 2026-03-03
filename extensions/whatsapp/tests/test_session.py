from __future__ import annotations

from pathlib import Path

from session import SessionConfig, SessionManager


def test_session_storage_and_self_id(tmp_path: Path):
    manager = SessionManager(SessionConfig(data_dir=tmp_path, account_id="acc1"))
    manager.ensure_storage()
    assert manager.config.session_db_path.exists()
    manager.persist_self_id("111@s.whatsapp.net")
    assert manager.read_self_id() == "111@s.whatsapp.net"
