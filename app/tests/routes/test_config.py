from __future__ import annotations

from pathlib import Path

from app.routes import config


def test_load_config_from_example_when_main_missing(tmp_path: Path) -> None:
    config_dir = tmp_path / "config"
    config_dir.mkdir(parents=True)
    (config_dir / "openagent.yaml.example").write_text("kind: openai_compat\n", encoding="utf-8")

    raw, error = config._load_config(tmp_path)
    assert "openai_compat" in raw
    assert error is not None


def test_load_config_valid_yaml(tmp_path: Path) -> None:
    config_dir = tmp_path / "config"
    config_dir.mkdir(parents=True)
    (config_dir / "openagent.yaml").write_text("kind: openai_compat\n", encoding="utf-8")

    raw, error = config._load_config(tmp_path)
    assert "openai_compat" in raw
    assert error is None


def test_load_config_invalid_yaml(tmp_path: Path) -> None:
    config_dir = tmp_path / "config"
    config_dir.mkdir(parents=True)
    (config_dir / "openagent.yaml").write_text("kind: [broken", encoding="utf-8")

    raw, error = config._load_config(tmp_path)
    assert "kind: [broken" in raw
    assert error is not None
