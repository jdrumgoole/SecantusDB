from __future__ import annotations

from pathlib import Path

from secantus.cli import _overrides_from_args, build_parser
from secantus.config import SecantusConfig, apply_overrides


def _resolve(argv: list[str]) -> SecantusConfig:
    """Helper: parse argv, return the resolved SecantusConfig as the
    daemon would actually see it. Argparse defaults are intentionally
    None now (the "user did not pass this" sentinel) — defaulting
    happens in SecantusConfig, so test assertions live there too."""
    args = build_parser().parse_args(argv)
    return apply_overrides(SecantusConfig(), _overrides_from_args(args))


def test_defaults() -> None:
    cfg = _resolve([])
    assert cfg.host == "127.0.0.1"
    assert cfg.port == 27017
    # On-disk default; ":memory:" is opt-in via the same flag.
    assert cfg.storage_path == "./secantus-data"
    assert cfg.log_level == "INFO"


def test_argparse_namespace_uses_none_sentinels() -> None:
    """Direct check that the argparse layer leaves user-not-passed
    values as None, so config-file values can take precedence over
    them in apply_overrides."""
    args = build_parser().parse_args([])
    assert args.host is None
    assert args.port is None
    assert args.storage_path is None
    assert args.log_level is None
    assert args.auth is None


def test_in_memory_opt_in() -> None:
    cfg = _resolve(["--storage-path", ":memory:"])
    assert cfg.storage_path == ":memory:"


def test_storage_path_overrides_default() -> None:
    cfg = _resolve(["--storage-path", "/var/lib/secantus/cellar"])
    assert cfg.storage_path == "/var/lib/secantus/cellar"


def test_all_flags_together() -> None:
    cfg = _resolve(
        [
            "--host",
            "0.0.0.0",
            "--port",
            "27018",
            "--storage-path",
            "/tmp/secantus-data",
            "--log-level",
            "DEBUG",
        ]
    )
    assert cfg.host == "0.0.0.0"
    assert cfg.port == 27018
    assert cfg.storage_path == "/tmp/secantus-data"
    assert cfg.log_level == "DEBUG"


def test_console_scripts_declared() -> None:
    # The daemons use the `secantusd-<engine>` scheme; the utilities carry the
    # bare `secantus-` import-name prefix. The old `secantusdb` / `secantus`
    # daemon aliases were removed (clean break — no backwards compatibility).
    pyproject = Path(__file__).resolve().parents[1] / "pyproject.toml"
    text = pyproject.read_text()
    assert 'secantusd-py = "secantus.cli:main"' in text
    assert 'secantusd-py-pg = "secantus.sql.pgserver:main"' in text
    assert 'secantus-admin = "secantus.admin.cli:main"' in text
    assert 'secantus-restore-archive = "secantus.restore_cli:main"' in text
    # The removed aliases must not reappear.
    assert 'secantusdb = "secantus.cli:main"' not in text
    assert 'secantus = "secantus.cli:main"' not in text
    assert "secantusdb-admin" not in text
    assert "secantusdb-restore-archive" not in text
