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


def test_stall_trigger_decision_logic() -> None:
    """The pure watchdog decision distinguishes all four cases in-process.

    The key new case is the last one: a worker crashed and the run is STILL
    going past the post-crash deadline even though progress is *recent* (the
    surviving workers keep reporting). The idle check alone returns quiet there —
    which is exactly how a crashed macOS shard crawled to its 90-min job cap.
    """
    import conftest  # the suite's own conftest, already on sys.path

    now = 10_000.0
    # Healthy: recent progress, no crash -> quiet.
    assert conftest._stall_trigger(now, now - 1, 300, None, 1200) is None
    # Idle stall: no report past the limit, no crash -> fires with the idle reason.
    idle_reason = conftest._stall_trigger(now, now - 301, 300, None, 1200)
    assert idle_reason is not None and "no test report" in idle_reason
    # Crashed but still WITHIN the post-crash deadline, progress recent ->
    # survivable (xdist may restart), stay quiet.
    assert conftest._stall_trigger(now, now - 1, 300, now - 100, 1200) is None
    # Crashed AND past the deadline with recent progress -> post-crash overrun
    # fires (the case the idle check alone misses).
    overrun = conftest._stall_trigger(now, now - 1, 300, now - 1201, 1200)
    assert overrun is not None and "post-crash overrun" in overrun


@pytest.mark.timeout(240)
def test_worker_crash_is_captured_immediately(tmp_path: Path) -> None:
    """A crashed xdist worker dumps its reason at once — not only if a stall follows.

    The first version recorded the crash but printed nothing unless the idle
    stall watcher later tripped; when the survivors kept reporting it never did,
    so a 90-min crawl carried zero evidence of *why* a worker died. A crash must
    surface in the log the moment it happens.
    """
    result = _run_nested_pytest(
        tmp_path,
        """
        import os


        def test_crash_the_worker():
            # A marker makes the crash happen exactly once: the restarted worker
            # re-runs this test, sees the marker, and passes — so the session
            # finishes instead of exhausting the restart budget.
            marker = os.path.join(os.path.dirname(__file__), ".crashed")
            if not os.path.exists(marker):
                open(marker, "w").close()
                os._exit(1)


        def test_a():
            assert True


        def test_b():
            assert True
        """,
        stall_seconds="120",
        xdist=True,
    )
    combined = result.stdout + result.stderr
    assert "xdist worker went down" in combined, combined
    assert "Controller thread stacks follow" in combined, combined


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


# --- crash-capture faulthandler (SECANTUS_FAULTHANDLER_DIR) -------------------


def _run_nested_with_fault_dir(
    tmp_path: Path,
    body: str,
    *,
    fault_dir: Path,
    xdist: bool,
    max_worker_restart: str = "0",
    timeout: int = 300,
) -> subprocess.CompletedProcess[str]:
    """Nested pytest session with ``SECANTUS_FAULTHANDLER_DIR`` armed."""
    (tmp_path / "conftest.py").write_text(_CONFTEST_UNDER_TEST.read_text())
    (tmp_path / "test_nested.py").write_text(textwrap.dedent(body))
    env = dict(os.environ)
    env["SECANTUS_FAULTHANDLER_DIR"] = str(fault_dir)
    cmd = [sys.executable, "-m", "pytest", str(tmp_path)]
    cmd += ["-n", "2"] if xdist else ["-p", "no:xdist"]
    cmd += ["-p", "no:randomly", "-p", "no:cacheprovider", "-o", "addopts=", "-q"]
    if xdist:
        cmd += ["--max-worker-restart", max_worker_restart]
    return subprocess.run(
        cmd, capture_output=True, text=True, timeout=timeout, cwd=tmp_path, env=env
    )


# These two tests each spawn a nested `pytest -n 2` subprocess. Under the full
# `-n auto` suite that oversubscribes CPU, and if BOTH run at once (each spawning
# its own nested session) the nested runs can miss their subprocess deadline and
# get SIGKILLed — a contention artifact, not a real crash-capture failure. Pin
# them to one xdist group so `--dist=loadgroup` serialises them onto a single
# outer worker (never two nested sessions at once), and give the nested spawn
# (300s) and the test deadline (360s > the spawn's) enough headroom for a slow
# start under load. See ci-check's "crash_stall_watchdog" catalog entry.
@pytest.mark.xdist_group("crash_watchdog_nested")
@pytest.mark.timeout(360)
def test_faulthandler_dir_arms_a_file_per_worker(tmp_path: Path) -> None:
    """With the dir set, every process arms its own ``faulthandler-<id>.log``.

    A clean run leaves the files empty (nothing crashed) — their existence is
    the proof that ``_arm_crash_faulthandler`` ran in the controller AND in each
    xdist worker, so a later crash in any of them has somewhere to dump.
    """
    fault_dir = tmp_path / "fh"
    result = _run_nested_with_fault_dir(
        tmp_path,
        """
        def test_ok():
            assert True
        """,
        fault_dir=fault_dir,
        xdist=True,
    )
    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    names = {p.name for p in fault_dir.glob("faulthandler-*.log")}
    assert "faulthandler-controller.log" in names, names
    assert any(n.startswith("faulthandler-gw") for n in names), (
        f"expected a per-worker file, got {names}"
    )


@pytest.mark.xdist_group("crash_watchdog_nested")
@pytest.mark.timeout(360)
def test_faulthandler_dir_captures_a_worker_crash(tmp_path: Path) -> None:
    """A hard crash (SIGSEGV) in an xdist worker leaves its stack in the file.

    This is the whole point: pytest's stderr faulthandler is lost when xdist
    reports "node down", but the file survives the dead worker. ``_sigsegv`` is
    faulthandler's own test hook — a real fatal signal, so it exercises the
    signal-handler path, not ``dump_traceback``.
    """
    fault_dir = tmp_path / "fh"
    result = _run_nested_with_fault_dir(
        tmp_path,
        """
        import faulthandler


        def test_boom():
            faulthandler._sigsegv()
        """,
        fault_dir=fault_dir,
        xdist=True,
    )
    # The worker crashed; the session fails. We assert on the FILE, not the code.
    assert result.returncode != 0
    dumps = [p.read_text() for p in fault_dir.glob("faulthandler-gw*.log") if p.read_text().strip()]
    assert dumps, (
        "no worker faulthandler file captured the crash; "
        f"dir={list(fault_dir.glob('*'))}\nstdout:\n{result.stdout}"
    )
    joined = "\n".join(dumps)
    assert "Fatal Python error" in joined or "Segmentation fault" in joined, joined[:400]
