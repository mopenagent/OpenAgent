from __future__ import annotations

from plugin import WhatsAppExtension


def test_plugin_status_snapshot_shape():
    ext = WhatsAppExtension(data_dir="data")
    status = ext.get_status()
    assert "running" in status
    assert "connected" in status
    assert "reconnect_attempts" in status
