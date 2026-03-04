from __future__ import annotations

import json
from pathlib import Path

from app.routes import services


def test_get_services_handles_valid_and_invalid_manifests(tmp_path: Path) -> None:
    good_dir = tmp_path / "services" / "good"
    bad_dir = tmp_path / "services" / "bad"
    good_dir.mkdir(parents=True)
    bad_dir.mkdir(parents=True)

    (good_dir / "service.json").write_text(
        json.dumps(
            {
                "name": "good",
                "description": "Good service",
                "version": "0.1.0",
                "socket": "data/sockets/good.sock",
                "tools": [],
            }
        ),
        encoding="utf-8",
    )
    (bad_dir / "service.json").write_text("{invalid", encoding="utf-8")

    result = services._get_services(tmp_path)
    assert len(result) == 2
    names = {svc["name"] for svc in result}
    assert "good" in names
    assert "bad" in names

    by_name = {svc["name"]: svc for svc in result}
    assert by_name["good"]["status"] == "stopped"
    assert by_name["bad"]["status"] == "error"


def test_get_services_empty_when_directory_missing(tmp_path: Path) -> None:
    assert services._get_services(tmp_path) == []
