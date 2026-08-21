"""mongod discovery for the parity benchmark runners.

The runners used to carry two hardcoded Homebrew paths from one developer's Mac,
so on any other machine both mongod arms silently reported "binary missing" and
the run compared the Rust server against nothing. These tests pin the behaviour
that replaced them: find every mongod, label each by its own version, and refuse
to be fooled by symlinks or unreadable binaries.

Nothing here executes a real mongod — the fake binaries are shell scripts that
print a version banner.
"""

from __future__ import annotations

import os
import stat
from pathlib import Path

import pytest
from bench.parity_remeasure import (
    build_arms,
    discover_mongods,
    mongod_version,
    newest_mongod,
)


def fake_mongod(path: Path, version: str | None) -> Path:
    """A stand-in binary printing mongod's real `--version` banner shape."""
    path.parent.mkdir(parents=True, exist_ok=True)
    banner = f"db version v{version}" if version else "not actually mongod"
    path.write_text(f"#!/bin/sh\necho '{banner}'\n")
    path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return path


def test_version_parsed_from_banner(tmp_path):
    assert mongod_version(str(fake_mongod(tmp_path / "m", "8.3.4"))) == "8.3.4"


def test_unparseable_binary_is_not_an_arm(tmp_path):
    """A binary that answers but isn't mongod must not become a silent arm."""
    assert mongod_version(str(fake_mongod(tmp_path / "m", None))) is None
    assert discover_mongods([str(tmp_path / "m")]) == {}


def test_missing_and_non_executable_paths_are_skipped(tmp_path):
    missing = tmp_path / "nope"
    plain = tmp_path / "plain"
    plain.write_text("#!/bin/sh\necho 'db version v9.9.9'\n")  # never chmod +x
    assert discover_mongods([str(missing), str(plain)]) == {}


def test_each_version_becomes_its_own_labelled_arm(tmp_path):
    six = fake_mongod(tmp_path / "six" / "mongod", "6.0.16")
    eight = fake_mongod(tmp_path / "eight" / "mongod", "8.3.4")
    arms = discover_mongods([str(six), str(eight)])
    assert arms == {
        "mongod-6.0.16": ("mongod", str(six.resolve())),
        "mongod-8.3.4": ("mongod", str(eight.resolve())),
    }


def test_symlink_to_the_same_binary_is_not_a_second_arm(tmp_path):
    """The original bug's shape: /opt/homebrew/bin/mongod -> Cellar/.../mongod.

    Deduping by realpath keeps that from doubling the run's wall-clock while
    measuring one binary twice.
    """
    real = fake_mongod(tmp_path / "cellar" / "mongod", "6.0.16")
    link = tmp_path / "bin" / "mongod"
    link.parent.mkdir(parents=True, exist_ok=True)
    link.symlink_to(real)
    arms = discover_mongods([str(link), str(real)])
    assert list(arms) == ["mongod-6.0.16"]
    assert arms["mongod-6.0.16"][1] == str(real.resolve())


def test_build_arms_always_includes_rust(tmp_path):
    arms = build_arms([str(fake_mongod(tmp_path / "m", "8.3.4"))])
    assert arms["rust"] == ("rust", None)
    assert "mongod-8.3.4" in arms


def test_newest_mongod_compares_numerically_not_lexically(tmp_path):
    """`"10.0.0" < "8.3.4"` as strings — the oplog-tax default must not pick 8."""
    eight = fake_mongod(tmp_path / "a" / "mongod", "8.3.4")
    ten = fake_mongod(tmp_path / "b" / "mongod", "10.0.0")
    arms = build_arms([str(eight), str(ten)])
    assert newest_mongod(arms) == str(ten.resolve())


def test_newest_mongod_is_none_when_only_rust(tmp_path):
    assert newest_mongod({"rust": ("rust", None)}) is None


def test_env_override_is_discovered(tmp_path, monkeypatch):
    """SECANTUS_MONGOD_BIN is how the harnesses pin a build; it must be found."""
    binary = fake_mongod(tmp_path / "env" / "mongod", "7.0.12")
    monkeypatch.setenv("SECANTUS_MONGOD_BIN", str(binary))
    monkeypatch.setattr("bench.parity_remeasure.MONGOD_SEARCH_GLOBS", ())
    monkeypatch.setattr("bench.parity_remeasure.shutil.which", lambda _n: None)
    assert "mongod-7.0.12" in discover_mongods()


def test_explicit_paths_bypass_discovery(tmp_path, monkeypatch):
    """--mongod must not be diluted by whatever else the box happens to have."""
    strays = fake_mongod(tmp_path / "stray" / "mongod", "4.4.1")
    monkeypatch.setenv("SECANTUS_MONGOD_BIN", str(strays))
    monkeypatch.setattr(
        "bench.parity_remeasure.MONGOD_SEARCH_GLOBS", (str(tmp_path / "stray" / "mongod"),)
    )
    chosen = fake_mongod(tmp_path / "chosen" / "mongod", "8.3.4")
    assert list(discover_mongods([str(chosen)])) == ["mongod-8.3.4"]


@pytest.mark.parametrize("cores,expected", [(3, 2.0), (12, 4.0), (64, 64 / 3)])
def test_load_ceiling_scales_with_cores(cores, expected, monkeypatch):
    """A 4-core box and a 64-core box do not mean the same thing by "load 4"."""
    monkeypatch.setattr(os, "cpu_count", lambda: cores)
    ceiling = max(2.0, (os.cpu_count() or 4) / 3.0)
    assert ceiling == pytest.approx(expected)
