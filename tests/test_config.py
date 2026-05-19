"""Tests for ``secantus.config`` — TOML loader, auto-discovery,
precedence (defaults < TOML < explicit CLI flag), and CLI wiring.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from secantus.cli import _overrides_from_args, build_parser
from secantus.config import (
    ConfigError,
    SecantusConfig,
    apply_overrides,
    discover_config_path,
    load_config,
)

# ---------------------------------------------------------------------------
# SecantusConfig defaults
# ---------------------------------------------------------------------------


def test_defaults_match_legacy_cli_defaults() -> None:
    """The dataclass defaults must equal the pre-config CLI defaults so
    `secantusdb` with no flags / no file behaves identically to before."""
    cfg = SecantusConfig()
    assert cfg.host == "127.0.0.1"
    assert cfg.port == 27017
    assert cfg.storage_path == "./secantus-data"
    assert cfg.log_level == "INFO"
    assert cfg.auth is False
    assert cfg.standalone is False
    assert cfg.noop_heartbeat_seconds == 0.0
    assert cfg.cache_size == "1G"
    assert cfg.session_max == 1000
    assert cfg.oplog_retention_seconds == 3600.0
    assert cfg.oplog_max_entries == 100_000
    assert cfg.ttl_sweep_seconds == 60.0
    assert cfg.sync_on_commit is False


# ---------------------------------------------------------------------------
# TOML parsing
# ---------------------------------------------------------------------------


def test_explicit_path_overrides_only_set_fields(tmp_path) -> None:
    p = tmp_path / "secantusdb.toml"
    p.write_text(
        """
        [server]
        host = "0.0.0.0"
        port = 5000
        auth = true

        [storage]
        cache_size = "4G"
        sync_on_commit = true
        """
    )
    cfg, source = load_config(p)
    assert source == p
    # Specified fields overridden.
    assert cfg.host == "0.0.0.0"
    assert cfg.port == 5000
    assert cfg.auth is True
    assert cfg.cache_size == "4G"
    assert cfg.sync_on_commit is True
    # Unspecified fields keep defaults.
    assert cfg.storage_path == "./secantus-data"
    assert cfg.session_max == 1000
    assert cfg.oplog_retention_seconds == 3600.0


def test_explicit_path_missing_raises(tmp_path) -> None:
    with pytest.raises(ConfigError, match="not found"):
        load_config(tmp_path / "does-not-exist.toml")


def test_rejects_unknown_top_level_table(tmp_path) -> None:
    p = tmp_path / "c.toml"
    p.write_text(
        """
        [bogus]
        x = 1
        """
    )
    with pytest.raises(ConfigError, match="unknown top-level table"):
        load_config(p)


def test_rejects_unknown_key_inside_known_table(tmp_path) -> None:
    """A typo like ``cache_seize`` would otherwise silently leave WT
    running with the default — fail loudly instead."""
    p = tmp_path / "c.toml"
    p.write_text(
        """
        [storage]
        cache_seize = "4G"
        """
    )
    with pytest.raises(ConfigError, match="unknown key"):
        load_config(p)


def test_rejects_table_that_is_not_a_table(tmp_path) -> None:
    p = tmp_path / "c.toml"
    p.write_text("storage = 42\n")
    with pytest.raises(ConfigError, match="must be a table"):
        load_config(p)


def test_rejects_invalid_toml(tmp_path) -> None:
    p = tmp_path / "c.toml"
    p.write_text("[server\nhost = 1.2.3.4")
    with pytest.raises(ConfigError, match="invalid TOML"):
        load_config(p)


def test_oplog_table_uses_renamed_keys(tmp_path) -> None:
    """``[oplog] retention_seconds`` writes the same field as
    ``oplog_retention_seconds`` at the dataclass level. The TOML
    nesting is what makes the rename worth the small mapping
    overhead in config.py."""
    p = tmp_path / "c.toml"
    p.write_text(
        """
        [oplog]
        retention_seconds = 7200
        max_entries = 5000
        noop_heartbeat_seconds = 10.0
        """
    )
    cfg, _ = load_config(p)
    assert cfg.oplog_retention_seconds == 7200
    assert cfg.oplog_max_entries == 5000
    assert cfg.noop_heartbeat_seconds == 10.0


# ---------------------------------------------------------------------------
# Auto-discovery
# ---------------------------------------------------------------------------


def test_auto_discovery_picks_cwd_first(tmp_path, monkeypatch) -> None:
    cwd_file = tmp_path / "secantusdb.toml"
    cwd_file.write_text('[server]\nhost = "from-cwd"\n')
    monkeypatch.chdir(tmp_path)
    cfg, source = load_config(None)
    assert source is not None
    assert source.resolve() == cwd_file.resolve()
    assert cfg.host == "from-cwd"


def test_auto_discovery_returns_defaults_when_no_file_present(tmp_path, monkeypatch) -> None:
    monkeypatch.chdir(tmp_path)
    # Point HOME at an empty dir so ~/.secantus/secantusdb.toml is
    # also missing. /etc/secantus/secantusdb.toml is left alone — if
    # someone running this test really has one there, that's their
    # config, and the test would fail; treat that as acceptable.
    monkeypatch.setenv("HOME", str(tmp_path / "empty-home"))
    cfg, source = load_config(None)
    assert cfg == SecantusConfig()
    # Source may be None (clean) or /etc/... if the developer's box
    # has one.
    assert source is None or source == Path("/etc/secantus/secantusdb.toml")


def test_discover_config_path_returns_cwd_when_present(tmp_path, monkeypatch) -> None:
    f = tmp_path / "secantusdb.toml"
    f.write_text("")
    monkeypatch.chdir(tmp_path)
    assert discover_config_path() == Path("secantusdb.toml")


# ---------------------------------------------------------------------------
# Precedence: defaults < TOML < explicit CLI flag
# ---------------------------------------------------------------------------


def test_cli_flag_beats_toml_value(tmp_path) -> None:
    p = tmp_path / "c.toml"
    p.write_text('[server]\nhost = "from-file"\nport = 5000\n')
    base, _ = load_config(p)
    # User typed --port 9000 but did not touch --host.
    args = build_parser().parse_args(["--config", str(p), "--port", "9000"])
    cfg = apply_overrides(base, _overrides_from_args(args))
    assert cfg.host == "from-file"  # TOML wins over default
    assert cfg.port == 9000  # CLI wins over TOML


def test_no_flags_no_file_keeps_defaults() -> None:
    args = build_parser().parse_args([])
    base = SecantusConfig()
    cfg = apply_overrides(base, _overrides_from_args(args))
    assert cfg == base


def test_flag_only_overrides_default() -> None:
    args = build_parser().parse_args(["--cache-size", "8G", "--sync-on-commit"])
    base = SecantusConfig()
    cfg = apply_overrides(base, _overrides_from_args(args))
    assert cfg.cache_size == "8G"
    assert cfg.sync_on_commit is True
    # Other fields unchanged.
    assert cfg.host == "127.0.0.1"
    assert cfg.port == 27017


def test_overrides_rejects_unknown_field() -> None:
    with pytest.raises(ConfigError, match="unknown field"):
        apply_overrides(SecantusConfig(), {"not_a_field": 42})


def test_tls_table_uses_renamed_keys(tmp_path) -> None:
    """``[tls] cert_file`` writes ``tls_cert_file`` at the dataclass
    level — the TOML reads cleanly without an awkward prefix."""
    p = tmp_path / "c.toml"
    p.write_text(
        """
        [tls]
        cert_file = "/etc/ssl/server.crt"
        key_file = "/etc/ssl/server.key"
        """
    )
    cfg, _ = load_config(p)
    assert cfg.tls_cert_file == "/etc/ssl/server.crt"
    assert cfg.tls_key_file == "/etc/ssl/server.key"


def test_tls_cli_flag_overrides_toml(tmp_path) -> None:
    p = tmp_path / "c.toml"
    p.write_text(
        """
        [tls]
        cert_file = "/etc/ssl/server.crt"
        key_file = "/etc/ssl/server.key"
        """
    )
    base, _ = load_config(p)
    args = build_parser().parse_args(
        ["--config", str(p), "--tls-cert-file", "/override/server.crt"]
    )
    cfg = apply_overrides(base, _overrides_from_args(args))
    assert cfg.tls_cert_file == "/override/server.crt"
    # TOML still wins where the CLI was silent.
    assert cfg.tls_key_file == "/etc/ssl/server.key"


# ---------------------------------------------------------------------------
# End-to-end: config-driven server
# ---------------------------------------------------------------------------


def test_storage_honors_cache_size_kwarg(tmp_path) -> None:
    """The Storage constructor accepts cache_size and the WT engine
    starts successfully on a non-default size."""
    from secantus.storage import Storage

    storage = Storage(str(tmp_path / "wt"), cache_size="256M", session_max=64)
    try:
        assert storage.cache_size == "256M"
        assert storage.session_max == 64
        # Sanity: WT actually opened and works.
        storage.insert("d", "c", [{"_id": 1, "v": 1}])
        assert list(storage.find_matching("d", "c", {})) == [{"_id": 1, "v": 1}]
    finally:
        storage.close()


def test_storage_honors_sync_on_commit_kwarg(tmp_path) -> None:
    """sync_on_commit=True opens WT with the per-commit-fsync config
    and the engine still works (correctness; durability is a deeper
    test outside this slice's scope)."""
    from secantus.storage import Storage

    storage = Storage(str(tmp_path / "wt"), sync_on_commit=True)
    try:
        assert storage.sync_on_commit is True
        storage.insert("d", "c", [{"_id": 1}])
        assert list(storage.find_matching("d", "c", {})) == [{"_id": 1}]
    finally:
        storage.close()
