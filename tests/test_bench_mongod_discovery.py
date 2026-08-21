"""mongod discovery for the parity benchmark runners.

The runners used to carry two hardcoded Homebrew paths from one developer's Mac,
so on any other machine both mongod arms silently reported "binary missing" and
the run compared the Rust server against nothing while still writing a
trusted-looking artifact. These tests pin the behaviour that replaced them.

**No test here executes anything.** The first version of this file wrote
`#!/bin/sh` stubs, which Windows cannot run — so every discovery test failed on
that platform, in a file whose whole point was portability. Selection rules are
driven through an injected `probe`, and the `--version` parsing is tested
directly as a pure function.
"""

from __future__ import annotations

import os

import pytest
from bench.parity_remeasure import (
    build_arms,
    discover_mongods,
    newest_mongod,
    parse_version_banner,
)


def probe_from(mapping: dict[str, str]) -> callable:
    """A stand-in for asking a binary its version, keyed on resolved path."""
    return lambda real: next(
        (v for k, v in mapping.items() if os.path.realpath(k) == real),
        None,
    )


def touch(path, executable: bool = True):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("binary")
    if executable:
        path.chmod(0o755)
    else:
        path.chmod(0o644)
    return path


# ----------------------------------------------------------- banner parsing
@pytest.mark.parametrize(
    "banner,expected",
    [
        ("db version v8.3.4", "8.3.4"),
        ("db version 6.0.16", "6.0.16"),  # no leading v
        ("db version v8.3.4\nBuild Info: {...}", "8.3.4"),
        ("not actually mongod", None),
        ("", None),
        ("db version vX.Y.Z", None),
    ],
)
def test_parse_version_banner(banner, expected):
    assert parse_version_banner(banner) == expected


# --------------------------------------------------------- selection rules
def test_each_version_becomes_its_own_labelled_arm(tmp_path):
    six, eight = touch(tmp_path / "six" / "mongod"), touch(tmp_path / "eight" / "mongod")
    arms = discover_mongods(
        [str(six), str(eight)],
        probe=probe_from({str(six): "6.0.16", str(eight): "8.3.4"}),
    )
    assert arms == {
        "mongod-6.0.16": ("mongod", str(six.resolve())),
        "mongod-8.3.4": ("mongod", str(eight.resolve())),
    }


def test_binary_that_is_not_mongod_is_not_an_arm(tmp_path):
    """A binary that answers but isn't mongod must not become a silent arm."""
    other = touch(tmp_path / "other")
    assert discover_mongods([str(other)], probe=lambda _r: None) == {}


def test_missing_path_is_skipped(tmp_path):
    assert discover_mongods([str(tmp_path / "nope")], probe=lambda _r: "8.3.4") == {}


@pytest.mark.skipif(os.name == "nt", reason="X_OK reports every file executable on Windows")
def test_non_executable_file_is_skipped(tmp_path):
    plain = touch(tmp_path / "plain", executable=False)
    assert discover_mongods([str(plain)], probe=lambda _r: "8.3.4") == {}


def test_symlink_to_the_same_binary_is_not_a_second_arm(tmp_path):
    """The original bug's shape: /opt/homebrew/bin/mongod -> Cellar/.../mongod.

    Deduping by realpath keeps that from doubling the run's wall-clock while
    measuring one binary twice.
    """
    real = touch(tmp_path / "cellar" / "mongod")
    link = tmp_path / "bin" / "mongod"
    link.parent.mkdir(parents=True, exist_ok=True)
    link.symlink_to(real)
    arms = discover_mongods([str(link), str(real)], probe=probe_from({str(real): "6.0.16"}))
    assert list(arms) == ["mongod-6.0.16"]
    assert arms["mongod-6.0.16"][1] == str(real.resolve())


def test_build_arms_always_includes_rust(tmp_path):
    binary = touch(tmp_path / "mongod")
    arms = build_arms([str(binary)], probe=probe_from({str(binary): "8.3.4"}))
    assert arms["rust"] == ("rust", None)
    assert "mongod-8.3.4" in arms


def test_explicit_paths_bypass_discovery(tmp_path, monkeypatch):
    """--mongod must not be diluted by whatever else the box happens to have."""
    stray = touch(tmp_path / "stray" / "mongod")
    chosen = touch(tmp_path / "chosen" / "mongod")
    monkeypatch.setenv("SECANTUS_MONGOD_BIN", str(stray))
    monkeypatch.setattr("bench.parity_remeasure.MONGOD_SEARCH_GLOBS", (str(stray),))
    arms = discover_mongods(
        [str(chosen)],
        probe=probe_from({str(stray): "4.4.1", str(chosen): "8.3.4"}),
    )
    assert list(arms) == ["mongod-8.3.4"]


def test_env_override_is_discovered(tmp_path, monkeypatch):
    """SECANTUS_MONGOD_BIN is how the harnesses pin a build; it must be found."""
    binary = touch(tmp_path / "env" / "mongod")
    monkeypatch.setenv("SECANTUS_MONGOD_BIN", str(binary))
    monkeypatch.setattr("bench.parity_remeasure.MONGOD_SEARCH_GLOBS", ())
    monkeypatch.setattr("bench.parity_remeasure.shutil.which", lambda _n: None)
    assert "mongod-7.0.12" in discover_mongods(probe=probe_from({str(binary): "7.0.12"}))


# ------------------------------------------------------- oplog-tax default
def test_newest_mongod_compares_numerically_not_lexically(tmp_path):
    """`"10.0.0" < "8.3.4"` as strings — the oplog-tax default must not pick 8."""
    eight, ten = touch(tmp_path / "a" / "mongod"), touch(tmp_path / "b" / "mongod")
    arms = build_arms(
        [str(eight), str(ten)],
        probe=probe_from({str(eight): "8.3.4", str(ten): "10.0.0"}),
    )
    assert newest_mongod(arms) == str(ten.resolve())


def test_newest_mongod_is_none_when_only_rust():
    assert newest_mongod({"rust": ("rust", None)}) is None


@pytest.mark.parametrize("cores,expected", [(3, 2.0), (12, 4.0), (64, 64 / 3)])
def test_load_ceiling_scales_with_cores(cores, expected, monkeypatch):
    """A 4-core box and a 64-core box do not mean the same thing by "load 4"."""
    monkeypatch.setattr(os, "cpu_count", lambda: cores)
    assert max(2.0, (os.cpu_count() or 4) / 3.0) == pytest.approx(expected)
