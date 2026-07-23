"""Tests for the Ops Board layered config + CLI resolution."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from secantus.opsboard import cli
from secantus.opsboard.config import OpsboardConfig


def test_defaults() -> None:
    cfg = OpsboardConfig.resolve(cli={}, env={}, config_path="/does/not/exist")
    assert cfg.host == "127.0.0.1"
    assert cfg.port == 0
    assert cfg.no_window is False
    assert cfg.repo_root is None


def test_precedence_cli_over_env_over_file_over_default(tmp_path: Path) -> None:
    config_file = tmp_path / "opsboard.json"
    OpsboardConfig(host="1.1.1.1", port=1111, no_window=True).save(config_file)

    # File beats default.
    cfg = OpsboardConfig.resolve(cli={}, env={}, config_path=config_file)
    assert (cfg.host, cfg.port, cfg.no_window) == ("1.1.1.1", 1111, True)

    # Env beats file.
    env = {"SECANTUS_OPSBOARD_PORT": "2222", "SECANTUS_OPSBOARD_HOST": "2.2.2.2"}
    cfg = OpsboardConfig.resolve(cli={}, env=env, config_path=config_file)
    assert (cfg.host, cfg.port) == ("2.2.2.2", 2222)

    # CLI beats env.
    cfg = OpsboardConfig.resolve(cli={"port": 3333}, env=env, config_path=config_file)
    assert cfg.port == 3333
    assert cfg.host == "2.2.2.2"  # env still supplies host (no CLI host)


def test_cli_none_values_do_not_override(tmp_path: Path) -> None:
    config_file = tmp_path / "opsboard.json"
    OpsboardConfig(port=1234).save(config_file)
    # A None CLI value means "unset" and must not clobber the saved value.
    cfg = OpsboardConfig.resolve(cli={"port": None, "host": None}, env={}, config_path=config_file)
    assert cfg.port == 1234


def test_no_window_bool_env_parsing() -> None:
    for truthy in ("1", "true", "yes", "on", "TRUE"):
        cfg = OpsboardConfig.resolve(
            cli={}, env={"SECANTUS_OPSBOARD_NO_WINDOW": truthy}, config_path="/nope"
        )
        assert cfg.no_window is True
    for falsy in ("0", "false", "no", ""):
        cfg = OpsboardConfig.resolve(
            cli={}, env={"SECANTUS_OPSBOARD_NO_WINDOW": falsy}, config_path="/nope"
        )
        assert cfg.no_window is False


def test_save_and_reload_roundtrip(tmp_path: Path) -> None:
    config_file = tmp_path / "opsboard.json"
    original = OpsboardConfig(host="9.9.9.9", port=42, no_window=True, repo_root="/r")
    original.save(config_file)
    on_disk = json.loads(config_file.read_text())
    assert on_disk["host"] == "9.9.9.9"
    reloaded = OpsboardConfig.resolve(cli={}, env={}, config_path=config_file)
    assert reloaded == original


def test_load_file_ignores_unknown_keys(tmp_path: Path) -> None:
    config_file = tmp_path / "opsboard.json"
    config_file.write_text(json.dumps({"port": 5, "bogus": "x"}))
    data = OpsboardConfig.load_file(config_file)
    assert data == {"port": 5}


def test_export_env_sets_jobkit_locations() -> None:
    cfg = OpsboardConfig(db_path="/db/here", log_dir="/logs/here")
    env: dict[str, str] = {}
    cfg.export_env(env)
    assert env["SECANTUS_OPSBOARD_DB"] == "/db/here"
    assert env["SECANTUS_OPSBOARD_LOGS"] == "/logs/here"


# --------------------------------------------------------------------------- #
# CLI surface
# --------------------------------------------------------------------------- #


def test_cli_print_config_exits_without_server(
    tmp_path: Path, capsys: pytest.CaptureFixture[str], monkeypatch: pytest.MonkeyPatch
) -> None:
    config_file = tmp_path / "opsboard.json"
    OpsboardConfig(port=8123).save(config_file)
    rc = cli.main(["--config", str(config_file), "--print-config"])
    assert rc == 0
    out = json.loads(capsys.readouterr().out)
    assert out["port"] == 8123


def test_cli_save_persists_resolved_config(tmp_path: Path) -> None:
    config_file = tmp_path / "opsboard.json"
    rc = cli.main(
        [
            "--config",
            str(config_file),
            "--host",
            "5.5.5.5",
            "--port",
            "77",
            "--save",
            "--print-config",
        ]
    )
    assert rc == 0
    saved = json.loads(config_file.read_text())
    assert saved["host"] == "5.5.5.5"
    assert saved["port"] == 77


def test_cli_token_env_beats_generated(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("SECANTUS_OPSBOARD_TOKEN", "env-token-xyz")
    token = cli._resolve_token(override=None, token_path=tmp_path / "tok")
    assert token == "env-token-xyz"
    assert not (tmp_path / "tok").exists()  # env token → nothing persisted
