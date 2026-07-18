"""The controller-side stall watchdog in ``tests/conftest.py``.

A worker crash that wedges the xdist *controller* is invisible to every timeout
the suite otherwise relies on: ``--timeout`` and ``--session-timeout`` are both
enforced inside the worker, and the session-scoped ``_hang_watchdog`` fixture
never runs in the controller at all. The result observed in CI was 85 minutes of
silence ending in a job-level kill with no diagnostics.

These tests drive the watchdog end to end by running real nested pytest sessions
under xdist, because the whole mechanism lives in controller/worker process
boundaries that cannot be faked in-process.

Both directions matter:
  * it MUST fire when the controller genuinely stops making progress, and
  * it MUST NOT fire for a crash the run recovers from — a false positive here
    would turn a self-healing flake into a hard CI failure.
"""

from __future__ import annotations

import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

# The watchdog's hard-exit code, from tests/conftest.py.
_STALL_EXIT_CODE = 70

_CONFTEST_UNDER_TEST = Path(__file__).parent / "conftest.py"


def _run_nested_pytest(
    tmp_path: Path, body: str, *, stall_seconds: str, timeout: int = 180
) -> subprocess.CompletedProcess[str]:
    """Run a throwaway pytest session under xdist in its own process tree.

    The nested session gets a copy of the real ``conftest.py`` so it exercises
    the actual hooks rather than a reimplementation. ``-p no:cacheprovider``
    keeps the temp tree clean; ``-p no:randomly`` keeps ordering deterministic.
    """
    (tmp_path / "conftest.py").write_text(_CONFTEST_UNDER_TEST.read_text())
    (tmp_path / "test_nested.py").write_text(textwrap.dedent(body))

    return subprocess.run(
        [
            sys.executable,
            "-m",
            "pytest",
            str(tmp_path),
            "-n",
            "2",
            "-p",
            "no:randomly",
            "-p",
            "no:cacheprovider",
            "-o",
            "addopts=",
            "--timeout=120",
            "--timeout-method=thread",
            "--max-worker-restart=3",
            "-q",
        ],
        capture_output=True,
        text=True,
        timeout=timeout,
        cwd=tmp_path,
        env={
            **_clean_env(),
            "SECANTUS_POST_CRASH_STALL_SECONDS": stall_seconds,
        },
    )


def _clean_env() -> dict[str, str]:
    import os

    env = dict(os.environ)
    env.pop("SECANTUS_POST_CRASH_STALL_SECONDS", None)
    return env


@pytest.mark.timeout(240)
def test_stall_after_worker_crash_fails_fast_with_stacks(tmp_path: Path) -> None:
    """A crashed worker plus a stalled controller exits non-zero WITH stacks.

    ``os._exit`` inside a test body kills the worker process outright, which is
    exactly the "node down: Not properly terminated" xdist reports. The sleeping
    test then supplies the stall: while it runs, no report reaches the
    controller, so with a 5s grace the watchdog must trip.

    Without the watchdog this session runs to the sleep's completion (or in the
    real CI case, forever) with nothing explaining why.
    """
    result = _run_nested_pytest(
        tmp_path,
        """
        import os
        import time


        def test_crash_the_worker():
            os._exit(1)


        def test_long_gap_with_no_reports():
            time.sleep(60)
        """,
        stall_seconds="5",
    )

    assert result.returncode == _STALL_EXIT_CODE, (
        f"expected the stall watchdog's exit code {_STALL_EXIT_CODE}, got "
        f"{result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    combined = result.stdout + result.stderr
    assert "xdist controller stalled after a worker crash" in combined, combined
    # The diagnostics are the entire point — a fast failure that says nothing
    # would be no better than the job-level timeout it replaces.
    assert "Controller thread stacks follow" in combined, combined
    assert "Thread" in combined or "File " in combined, (
        "expected a faulthandler thread dump in the output:\n" + combined
    )


@pytest.mark.timeout(240)
def test_healthy_session_never_arms_the_watchdog(tmp_path: Path) -> None:
    """A session with no worker crash must be completely unaffected.

    This is the false-positive guard, and it is the ONLY safe shape for one.
    The obvious alternative — crash a worker, assert the run recovers without
    the watchdog firing — cannot be written as a stable test: measured over
    repeated runs, xdist's recovery from ``os._exit`` took 0.6s, 0.6s, then
    17s, and under full-suite load wedged past 180s entirely (which is the very
    bug this watchdog exists for). A test asserting "recovery is clean" would
    therefore be asserting something that is not reliably true, and a flaky
    test guarding against flakiness is worse than no test at all.

    So the guarantee pinned here is the one that IS deterministic: no node-down
    means ``pytest_testnodedown`` never fires, nothing arms, and a tiny 1s grace
    — far below the runtime of the session — still produces a clean exit.
    """
    result = _run_nested_pytest(
        tmp_path,
        """
        import time


        def test_a():
            time.sleep(2)


        def test_b():
            time.sleep(2)


        def test_c():
            assert True
        """,
        # 1s: if arming were unconditional rather than crash-triggered, the
        # sleeps alone would blow this grace and trip the watchdog.
        stall_seconds="1",
    )

    assert result.returncode == 0, (
        f"a healthy session should exit 0, got {result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    combined = result.stdout + result.stderr
    assert "xdist controller stalled" not in combined, combined
    assert "3 passed" in combined, combined
