from __future__ import annotations

import json
from pathlib import Path

from app.routes import dashboard


def test_system_stats_shape() -> None:
    stats = dashboard._system_stats()
    assert "cpu_pct" in stats
    assert "ram_pct" in stats
    assert "disk_pct" in stats
    assert "uptime" in stats


def test_discover_services_reads_manifests(tmp_path: Path) -> None:
    svc_dir = tmp_path / "services" / "demo"
    svc_dir.mkdir(parents=True)
    (svc_dir / "service.json").write_text(
        json.dumps(
            {
                "name": "demo",
                "description": "Demo svc",
                "version": "0.1.0",
            }
        ),
        encoding="utf-8",
    )
    services = dashboard._discover_services(tmp_path)
    assert len(services) == 1
    assert services[0]["name"] == "demo"
    assert services[0]["status"] == "stopped"
