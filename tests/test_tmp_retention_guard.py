"""The conftest guard must reject unsafe ``tmp_path_retention_policy`` values.

``failed`` / ``none`` delete a passed test's tmp dir mid-session, which races
WiredTiger's background eviction/log threads and triggers ``WT_PANIC`` under
CI's higher worker count (see ``tests/conftest.py::pytest_configure``). The
guard turns that into an up-front, unmissable error instead of a mid-run
native crash.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

import pytest

_REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent


def _nested_pytest(tmp_path: pathlib.Path, *extra: str) -> subprocess.CompletedProcess:
    """Run a collect-only pytest in a subprocess, with its OWN basetemp.

    ``--basetemp`` is load-bearing, not tidiness. A pytest given none picks its
    base directory through ``make_numbered_dir_with_cleanup``, which registers
    an ``atexit`` rmtree of every stale ``$TMPDIR/pytest-of-<user>/pytest-NNNN``
    but the newest three. With a backlog of those (this suite leaves WiredTiger
    stores behind), that cleanup runs for minutes AFTER the nested tests have
    all passed — the process simply never exits. Under ``-n auto`` the outer
    worker is killed waiting for it and xdist reports only ``node down: Not
    properly terminated``, naming no test and printing no traceback. An
    explicit basetemp skips the cleanup hook entirely.

    The ``timeout`` matters for the same reason: without one a hang wedges the
    worker silently instead of failing with something a reader can act on.
    (Same diagnosis and same fix as the nested runs in
    ``tests/test_crash_stall_watchdog.py``.)

    ``SECANTUS_NO_TMP_REAP=1`` is the same lesson one hook later. The nested
    run is a pytest CONTROLLER, so ``tests/conftest.py``'s sessionstart
    sweep (``_reap_abandoned_pytest_tmp``) ran in it too, ``rmtree``-ing
    every abandoned run's WiredTiger stores (~100 GiB a run) before
    collecting one file -- inside this test's 300 s budget, under the full
    suite's I/O. That is the 2026-09-30 timeout (no output for 300 s, 0.5 s
    alone); batch 64 reproduced it in miniature (a 100k-file stale tree:
    0.5 s -> 5.6 s, reaped by the nested run). The outer run's controller
    already sweeps.
    """
    env = dict(os.environ)
    env["SECANTUS_NO_TMP_REAP"] = "1"
    return subprocess.run(
        [
            sys.executable,
            "-m",
            "pytest",
            "-o",
            "addopts=",
            *extra,
            "--basetemp",
            str(tmp_path / "basetemp"),
            "--co",
            "-q",
            "tests/test_smoke.py",
        ],
        cwd=str(_REPO_ROOT),
        capture_output=True,
        text=True,
        timeout=300,
        env=env,
    )


@pytest.mark.parametrize("policy", ["failed", "none"])
def test_unsafe_tmp_retention_policy_is_rejected(policy: str, tmp_path) -> None:
    # Collect-only (fast) run of a real tests/ file so tests/conftest.py — and
    # thus the guard's pytest_configure — loads. addopts is cleared so the
    # subprocess doesn't inherit -n auto / markers.
    proc = _nested_pytest(tmp_path, "-o", f"tmp_path_retention_policy={policy}")
    assert proc.returncode != 0, f"policy={policy!r} was not rejected"
    assert "unsafe for this suite" in (proc.stdout + proc.stderr)


def test_default_tmp_retention_policy_is_allowed(tmp_path) -> None:
    proc = _nested_pytest(tmp_path)
    assert proc.returncode == 0, f"default policy rejected: {proc.stdout}\n{proc.stderr}"
    assert "unsafe for this suite" not in (proc.stdout + proc.stderr)
