"""The session stall watchdog in ``tests/conftest.py``.

A pytest run that wedges after its last test is invisible to every timeout the
suite otherwise relies on: ``--timeout`` and ``--session-timeout`` are both
enforced inside the worker, and the session-scoped ``_hang_watchdog`` fixture
never runs in the xdist controller at all. Observed in CI as 85 minutes of
silence ending in a job-level kill with no diagnostics.

These tests drive the watchdog end to end through real nested pytest sessions,
because the mechanism lives in process boundaries that cannot be faked
in-process.
"""

from __future__ import annotations

import os
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

# The watchdog's hard-exit code, from tests/conftest.py.
_STALL_EXIT_CODE = 70

_CONFTEST_UNDER_TEST = Path(__file__).parent / "conftest.py"


def _run_nested_pytest(
    tmp_path: Path,
    body: str,
    *,
    stall_seconds: str,
    xdist: bool,
    timeout: int = 180,
) -> subprocess.CompletedProcess[str]:
    """Run a throwaway pytest session in its own process tree.

    The nested session gets a copy of the real ``conftest.py`` so it exercises
    the actual hooks rather than a reimplementation.
    """
    (tmp_path / "conftest.py").write_text(_CONFTEST_UNDER_TEST.read_text())
    (tmp_path / "test_nested.py").write_text(textwrap.dedent(body))

    env = dict(os.environ)
    env["SECANTUS_STALL_SECONDS"] = stall_seconds

    cmd = [sys.executable, "-m", "pytest", str(tmp_path)]
    cmd += ["-n", "2"] if xdist else ["-p", "no:xdist"]
    cmd += [
        "-p",
        "no:randomly",
        "-p",
        "no:cacheprovider",
        "-o",
        "addopts=",
        "--timeout=120",
        "--timeout-method=thread",
        "-q",
    ]
    if xdist:
        cmd.append("--max-worker-restart=3")

    return subprocess.run(
        cmd, capture_output=True, text=True, timeout=timeout, cwd=tmp_path, env=env
    )


@pytest.mark.timeout(240)
def test_stall_fails_fast_with_stacks(tmp_path: Path) -> None:
    """A session that stops reporting exits non-zero WITH stacks.

    The sleeping test supplies the stall: while it runs, no report reaches the
    controller, so with a 5s limit the watchdog must trip. Without the watchdog
    this session simply runs to the sleep's end (or, in the real CI case,
    forever) with nothing explaining why.

    Note this deliberately does NOT crash a worker. The first version of the
    watchdog armed only on ``pytest_testnodedown``, and would have sat silent
    through exactly this scenario — which is the one later observed in the wild.
    """
    result = _run_nested_pytest(
        tmp_path,
        """
        import time


        def test_long_gap_with_no_reports():
            time.sleep(60)
        """,
        stall_seconds="5",
        xdist=True,
    )

    assert result.returncode == _STALL_EXIT_CODE, (
        f"expected the stall watchdog's exit code {_STALL_EXIT_CODE}, got "
        f"{result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    combined = result.stdout + result.stderr
    assert "pytest session stalled" in combined, combined
    # The diagnostics are the entire point — a fast failure that says nothing
    # would be no better than the job-level timeout it replaces.
    assert "Thread stacks follow" in combined, combined
    assert "no worker reported going down" in combined, combined


@pytest.mark.timeout(240)
def test_healthy_session_is_untouched(tmp_path: Path) -> None:
    """A run that keeps reporting is never killed, even with a tight limit.

    Runs SERIALLY on purpose. The xdist shutdown wedge this watchdog exists to
    catch also strikes healthy sessions — a nested xdist run was observed
    printing "3 passed in 2.52s" and then hanging until SIGKILL with no crash
    involved — so "a healthy xdist session exits 0" is not reliably true, and
    asserting it made an earlier version of this test flaky. Serial has no
    controller/worker split and therefore no wedge, which leaves this test
    pinning exactly one thing, deterministically: progress keeps the watchdog
    quiet.

    The 20s limit sits comfortably above the 8s gap but far below the ~300s
    floor, so an unconditional or elapsed-time watchdog would trip here.
    """
    result = _run_nested_pytest(
        tmp_path,
        """
        import time


        def test_a():
            time.sleep(8)


        def test_b():
            time.sleep(1)


        def test_c():
            assert True
        """,
        stall_seconds="20",
        xdist=False,
    )

    assert result.returncode == 0, (
        f"a healthy session should exit 0, got {result.returncode}\n"
        f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    )
    combined = result.stdout + result.stderr
    assert "pytest session stalled" not in combined, combined
    assert "3 passed" in combined, combined


def test_threshold_is_derived_from_the_per_test_timeout() -> None:
    """The limit must never sit below the per-test deadline.

    The watcher measures time since the last report, so while a single long
    test runs it cannot distinguish "slow" from "stalled". pytest kills any test
    at ``--timeout``, so that deadline is the floor below which a healthy run
    could be misread as a stall. A fixed 300s would have false-fired on a
    default local run, where the ini deadline is 600s.
    """
    import conftest  # the suite's own conftest, already on sys.path

    assert conftest._STALL_FLOOR_SECONDS >= 300
    assert conftest._STALL_TIMEOUT_MULTIPLIER > 1.0, (
        "the limit must exceed the per-test timeout, or a test that legitimately "
        "runs to its deadline would be reported as a stall"
    )
    # CI runs --timeout=120 -> the 300s floor; a default local run has ini
    # timeout=600 -> 1500s, which a fixed 300s limit would have violated.
    assert 600 * conftest._STALL_TIMEOUT_MULTIPLIER > 600
