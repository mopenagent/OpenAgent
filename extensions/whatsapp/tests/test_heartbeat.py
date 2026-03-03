from __future__ import annotations

from heartbeat import HeartbeatTracker


def test_heartbeat_snapshot_tracks_lifecycle():
    tracker = HeartbeatTracker()
    tracker.mark_running(True)
    tracker.set_linked(linked=True, auth_age_ms=123, self_id="111@s.whatsapp.net")
    tracker.mark_connected(self_id="111@s.whatsapp.net")
    tracker.mark_message()
    tracker.mark_disconnected(reason="network", code=500)
    tracker.mark_error("boom")
    snap = tracker.snapshot()
    assert snap.running is True
    assert snap.linked is True
    assert snap.connected is False
    assert snap.self_id == "111@s.whatsapp.net"
    assert snap.last_disconnect is not None
    assert snap.last_disconnect.code == 500
    assert snap.last_error == "boom"
