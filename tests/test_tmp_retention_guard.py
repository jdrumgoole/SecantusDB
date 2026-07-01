"""The conftest guard must reject unsafe ``tmp_path_retention_policy`` values.

``failed`` / ``none`` delete a passed test's tmp dir mid-session, which races
WiredTiger's background eviction/log threads and triggers ``WT_PANIC`` under
CI's higher worker count (see ``tests/conftest.py::pytest_configure``). The
guard turns that into an up-front, unmissable error instead of a mid-run
native crash.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

import pytest

_REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent


@pytest.mark.parametrize("policy", ["failed", "none"])
def test_unsafe_tmp_retention_policy_is_rejected(policy: str) -> None:
    # Collect-only (fast) run of a real tests/ file so tests/conftest.py — and
    # thus the guard's pytest_configure — loads. addopts is cleared so the
    # subprocess doesn't inherit -n auto / markers.
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "pytest",
            "-o",
            "addopts=",
            "-o",
            f"tmp_path_retention_policy={policy}",
            "--co",
            "-q",
            "tests/test_smoke.py",
        ],
        cwd=str(_REPO_ROOT),
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0, f"policy={policy!r} was not rejected"
    assert "unsafe for this suite" in (proc.stdout + proc.stderr)


def test_default_tmp_retention_policy_is_allowed() -> None:
    proc = subprocess.run(
        [sys.executable, "-m", "pytest", "-o", "addopts=", "--co", "-q", "tests/test_smoke.py"],
        cwd=str(_REPO_ROOT),
        capture_output=True,
        text=True,
    )
    assert proc.returncode == 0, f"default policy rejected: {proc.stdout}\n{proc.stderr}"
    assert "unsafe for this suite" not in (proc.stdout + proc.stderr)
