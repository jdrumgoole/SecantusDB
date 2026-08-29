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

import contextlib
import getpass
import os
import re
import signal
import subprocess
import sys
import tempfile
import textwrap
from collections.abc import Iterator
from pathlib import Path

try:
    import fcntl  # POSIX-only; absent on Windows.
except ImportError:  # pragma: no cover - Windows
    fcntl = None  # type: ignore[assignment]

import pytest

# The watchdog's hard-exit code, from tests/conftest.py.
_STALL_EXIT_CODE = 70

_CONFTEST_UNDER_TEST = Path(__file__).parent / "conftest.py"


def _isolated_basetemp_args(tmp_path: Path) -> list[str]:
    """``--basetemp`` args that keep a nested session out of the SHARED temp root.

    Without ``--basetemp``, every pytest process picks its base directory through
    ``make_numbered_dir_with_cleanup``, which **registers an atexit hook** that
    ``rmtree``s every stale ``$TMPDIR/pytest-of-<user>/pytest-NNNN`` except the
    newest three (``_pytest/pathlib.py``). On a dev box that backlog is the
    leftovers of every earlier suite run — hundreds of trees full of WiredTiger
    databases — so the hook grinds through millions of ``unlink()`` calls inside
    ``Py_FinalizeEx``, minutes AFTER the session printed its summary.

    Measured 2026-08-17 against this file's own nested command, with 238 stale
    numbered dirs in the shared root: nested tests 0.55s, process wall clock
    **252.96s**, and ``sample`` of the wedged pid showed 1479 of 1559 samples in
    ``Py_FinalizeEx → atexit_callfuncs → os.unlink``. That is the long-standing
    "every nested test passed, then the process never exited" hang that
    SIGKILLed these tests at their subprocess budget. It is intermittent because
    the first run to pay the cost drains the backlog for the next one — the same
    command three times in a row took 252.96s, 11.95s, 0.60s.

    An explicit basetemp skips ``make_numbered_dir_with_cleanup`` altogether
    (``TempPathFactory.getbasetemp``), so a nested run neither pays for nor adds
    to that backlog: its temp tree lives under the outer test's ``tmp_path`` and
    is cleaned up with it. xdist workers were always immune — xdist hands each
    one ``--basetemp <controller basetemp>/popen-gwN`` — so only the nested
    CONTROLLER ever wedged, and it wedged *after* writing its summary, which is
    the post-summary shape ``_run_tolerating_teardown_wedge`` was built for.
    """
    return ["--basetemp", str(tmp_path / "basetemp")]


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
    cmd += _isolated_basetemp_args(tmp_path)
    if xdist:
        cmd.append("--max-worker-restart=3")

    return subprocess.run(
        cmd, capture_output=True, text=True, timeout=timeout, cwd=tmp_path, env=env
    )


@pytest.mark.timeout(300)
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


@pytest.mark.timeout(300)
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


@pytest.mark.timeout(300)
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
    timeout: int = 200,
) -> subprocess.CompletedProcess[str]:
    """Nested pytest session with ``SECANTUS_FAULTHANDLER_DIR`` armed.

    The nested-run budget (200s) is generous on purpose, but it is NOT there to
    absorb CPU starvation — that theory was measured and refuted. What used to
    eat the budget was pytest's shared-temp-root cleanup running in the nested
    session's ``atexit`` (see ``_isolated_basetemp_args``, which now removes it:
    252.96s of ``unlink()`` for a 0.55s test run). The outer
    ``@pytest.mark.timeout(360)`` marks leave room for this budget plus the
    post-kill verdict parse."""
    (tmp_path / "conftest.py").write_text(_CONFTEST_UNDER_TEST.read_text())
    (tmp_path / "test_nested.py").write_text(textwrap.dedent(body))
    env = dict(os.environ)
    env["SECANTUS_FAULTHANDLER_DIR"] = str(fault_dir)
    cmd = [sys.executable, "-m", "pytest", str(tmp_path)]
    cmd += ["-n", "2"] if xdist else ["-p", "no:xdist"]
    cmd += ["-p", "no:randomly", "-p", "no:cacheprovider", "-o", "addopts=", "-q"]
    cmd += _isolated_basetemp_args(tmp_path)
    if xdist:
        cmd += ["--max-worker-restart", max_worker_restart]
    with _nested_run_lock():
        return _run_tolerating_teardown_wedge(cmd, cwd=tmp_path, env=env, timeout=timeout)


# The final summary pytest prints once its session is over — everything these
# tests assert on (the faulthandler files and the pass/fail verdict) is decided
# by the time it appears. The nested runs use ``-q``, where the summary is a
# BARE line ("1 passed in 0.53s"), not the ``=== ... ===`` banner — an earlier
# banner-only pattern here never matched ``-q`` output, so the post-summary
# wedge tolerance below was dead code and every teardown wedge escalated to
# TimeoutExpired (2026-07-31 release-gate failure). Match both forms.
_PYTEST_SUMMARY = re.compile(
    r"^=*\s*(?P<body>[^=\n]*\b(?:passed|failed|error|errors|no tests ran)\b"
    r"[^=\n]*? in [\d.]+s)[^\n]*$",
    re.M,
)

# pytest's progress percentage reaches 100% only once EVERY test has been
# reported, so it proves the session finished its run even when the wedge
# beats the summary line to stdout. Observed 2026-08-17: a nested run's
# captured stdout was `bringing up nodes...` + `.` + `[100%]` and then the
# process never exited — the summary-only tolerance below could not match it
# and every occurrence escalated to a release-blocking TimeoutExpired.
_PYTEST_PROGRESS_DONE = re.compile(r"\[\s*100%\]")

#: Unambiguous whole-output markers that the nested session had a crash.
_PYTEST_CRASH_MARKERS = ("node down", "crashed", "internal error", "INTERNALERROR")


def _progress_says_failed(text: str) -> bool:
    """Whether pytest's ``-q`` progress output reports a failure.

    Only the progress CHARACTERS are inspected (the run of ``.``/``F``/``E``/
    ``s``/``x`` before the percentage), never the whole stream — a tmp path or
    a warning line containing an ``F`` must not read as a failed test.
    """
    for line in text.splitlines():
        m = _PYTEST_PROGRESS_DONE.search(line)
        if m is None:
            continue
        marks = line[: m.start()].strip()
        if any(c in marks for c in "FE"):
            return True
    return any(mark in text for mark in _PYTEST_CRASH_MARKERS)


def _kill_process_group(proc: subprocess.Popen[str]) -> None:
    """SIGKILL the nested session *and* its xdist workers.

    ``proc.kill()`` kills only the controller. Its workers are grandchildren that
    inherited the stdout/stderr PIPES, so an orphaned worker holds the write ends
    open and the recovery ``communicate()`` never sees EOF — it times out too and
    the harness then reports empty output, discarding the very progress marks the
    tolerance below reads its verdict from. One ``killpg`` (the Popen above asks
    for its own session) reaps the whole nested tree instead.
    """
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
    except (OSError, AttributeError):  # already reaped, or no killpg (Windows)
        proc.kill()


def _run_tolerating_teardown_wedge(
    cmd: list[str], *, cwd: Path, env: dict[str, str], timeout: int
) -> subprocess.CompletedProcess[str]:
    """Run the nested pytest, tolerating a *post-summary* teardown wedge.

    A nested ``pytest -n 2`` — especially after a worker SIGSEGV — can print its
    summary and then hang in interpreter / xdist shutdown without ever exiting.
    That is the very "wedged after its last test" case the crash watchdog exists
    to detect, so it is not a test failure here: the session finished, the
    faulthandler files are written, and the verdict is recoverable. When it
    happens, kill the hung process and synthesize the return code instead of
    raising ``TimeoutExpired``.

    The wedge does not always beat the summary to stdout — it can land BETWEEN
    the last test being reported and the summary being written, which a
    summary-only tolerance misses. So the completion evidence is either the
    summary line or pytest's ``[100%]`` progress marker (which only appears
    once every test has been reported); with the marker alone the return code
    comes from the failure markers in the progress output. A hang with
    *neither* (genuinely stuck mid-run) still raises — that is a real failure.
    """
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        cwd=cwd,
        env=env,
        # Own process group so the kill below reaps the nested xdist WORKERS too.
        start_new_session=True,
    )
    try:
        out, err = proc.communicate(timeout=timeout)
        return subprocess.CompletedProcess(cmd, proc.returncode, out, err)
    except subprocess.TimeoutExpired:
        _kill_process_group(proc)
        try:
            out, err = proc.communicate(timeout=30)
        except subprocess.TimeoutExpired:
            out, err = "", ""
        text = out or ""
        m = _PYTEST_SUMMARY.search(text)
        if m is not None:
            summary = m.group("body")
            rc = 1 if ("failed" in summary or "error" in summary) else 0
            return subprocess.CompletedProcess(cmd, rc, out, err)
        if _PYTEST_PROGRESS_DONE.search(text):
            # Every test was reported, then the session wedged before writing
            # its summary. Read the verdict off the progress output instead.
            rc = 1 if _progress_says_failed(text) else 0
            return subprocess.CompletedProcess(cmd, rc, out, err)
        raise  # neither summary nor a finished run → a real hang


# The nested `pytest -n 2` sessions below spawn several extra processes each. If
# two of these tests run at once they oversubscribe the CPU and the nested run
# can miss its subprocess deadline and get SIGKILLed — a contention artifact, not
# a real crash-capture failure. The `xdist_group` marker alone does NOT prevent
# this: it only serialises under `--dist loadgroup`, and the suite runs plain
# `-n auto` (== `--dist load`), which ignores groups. So serialise for real with
# a machine-wide advisory file lock — held across every xdist worker and the
# controller, regardless of dist mode — so only one nested session runs at a time.
# (Windows has no `fcntl.flock`; there the lock degrades to prior behaviour.)
_NESTED_RUN_LOCK = Path(tempfile.gettempdir()) / "secantus-crash-watchdog-nested.lock"


@contextlib.contextmanager
def _nested_run_lock() -> Iterator[None]:
    if fcntl is None:  # Windows: no flock — fall back to prior (unserialised) behaviour.
        yield
        return
    with open(_NESTED_RUN_LOCK, "w") as fh:
        fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(fh.fileno(), fcntl.LOCK_UN)


# These two tests each spawn a nested `pytest -n 2` subprocess.
# `_run_nested_with_fault_dir` holds a machine-wide file lock (`_nested_run_lock`)
# so no two nested sessions ever run at once — the real serialisation, effective
# under any xdist dist mode — and runs the nested session through
# `_run_tolerating_teardown_wedge`, which treats a *post-summary* hang (the
# nested pytest printing its verdict, then wedging in xdist shutdown) as a
# finished session rather than a timeout. That wedge is intermittent and is
# exactly the failure mode the crash watchdog exists to catch, so it must not
# flake these tests. The `xdist_group` marker is a scheduling hint (only active
# under `--dist loadgroup`). See ci-check's "crash_stall_watchdog" catalog entry.
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


@pytest.mark.skipif(
    sys.platform.startswith("win"), reason="TMPDIR steers the temp root on POSIX only"
)
def test_nested_sessions_stay_out_of_the_shared_pytest_temp_root(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A nested session must not enrol in pytest's SHARED numbered-tmp cleanup.

    This is the root cause of the long-standing intermittent hang in the two
    tests above. Without ``--basetemp`` a pytest process picks its base dir via
    ``make_numbered_dir_with_cleanup``, which registers an ``atexit`` hook that
    ``rmtree``s every stale ``$TMPDIR/pytest-of-<user>/pytest-NNNN`` bar the
    newest three. With the hundreds of WiredTiger-laden leftovers a dev box
    accumulates, that hook ran for **252.96s** after a nested run whose tests
    took 0.55s — ``sample`` of the wedged pid put 1479 of 1559 samples in
    ``Py_FinalizeEx → atexit_callfuncs → os.unlink``. Every nested test passes,
    then the process never exits: exactly the reported symptom, and intermittent
    because the first run to pay the cost drains the backlog (252.96s, then
    11.95s, then 0.60s for the same command).

    Pinned with sentinel stale dirs in a private temp root: an enrolled session
    deletes all but the newest three of them, an isolated one leaves all five.
    Serial (``xdist=False``) on purpose — the enrolment is controller-side, so
    this proves it in ~1s without spinning up a second worker pair. The nested
    test asks for ``tmp_path`` so the session really does build a basetemp.
    """
    temp_root = tmp_path / "tmproot"
    shared = temp_root / f"pytest-of-{getpass.getuser()}"
    shared.mkdir(parents=True)
    shared.chmod(0o700)  # pytest rejects a group/world-readable shared root
    # More than keep=3 dirs, so the low-numbered ones are cleanup candidates.
    stale = [shared / f"pytest-{n}" for n in range(1, 6)]
    for d in stale:
        (d / "leftover").mkdir(parents=True)
    monkeypatch.setenv("TMPDIR", str(temp_root))

    run_dir = tmp_path / "run"
    run_dir.mkdir()
    result = _run_nested_with_fault_dir(
        run_dir,
        """
        def test_ok(tmp_path):
            assert tmp_path.is_dir()
        """,
        fault_dir=run_dir / "fh",
        xdist=False,
    )
    assert result.returncode == 0, f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
    deleted = [d.name for d in stale if not d.exists()]
    assert not deleted, (
        f"the nested session enrolled in the SHARED temp root and deleted {deleted}; "
        "on a real dev box that atexit rmtree runs for minutes after the last test "
        "and wedges the run past its subprocess budget"
    )


# --- the wedge tolerance itself ------------------------------------------- #


def test_wedge_tolerance_accepts_a_finished_run_without_a_summary(tmp_path: Path) -> None:
    """A session that reported every test then hung before its summary is a
    post-completion wedge, not a mid-run hang.

    Observed 2026-08-17: the nested run's stdout was ``bringing up nodes...``
    + ``.`` + ``[100%]`` and the process never exited, so the summary-only
    tolerance escalated to TimeoutExpired and blocked the suite.
    """
    script = tmp_path / "wedge.py"
    script.write_text(
        "import sys, time\n"
        "sys.stdout.write('bringing up nodes...\\n\\n.')\n"
        "sys.stdout.write(' ' * 40 + '[100%]\\n')\n"
        "sys.stdout.flush()\n"
        "time.sleep(300)\n"
    )
    result = _run_tolerating_teardown_wedge(
        [sys.executable, str(script)], cwd=tmp_path, env=dict(os.environ), timeout=5
    )
    assert result.returncode == 0
    assert "[100%]" in result.stdout


def test_wedge_tolerance_reports_failure_from_progress_marks(tmp_path: Path) -> None:
    script = tmp_path / "wedge_fail.py"
    script.write_text(
        "import sys, time\n"
        "sys.stdout.write('.F' + ' ' * 40 + '[100%]\\n')\n"
        "sys.stdout.flush()\n"
        "time.sleep(300)\n"
    )
    result = _run_tolerating_teardown_wedge(
        [sys.executable, str(script)], cwd=tmp_path, env=dict(os.environ), timeout=5
    )
    assert result.returncode == 1


def test_wedge_tolerance_still_raises_on_a_mid_run_hang(tmp_path: Path) -> None:
    """No summary AND no finished run — a genuine hang stays a failure."""
    script = tmp_path / "stuck.py"
    script.write_text(
        "import sys, time\nsys.stdout.write('bringing up nodes...\\n')\n"
        "sys.stdout.flush()\ntime.sleep(300)\n"
    )
    with pytest.raises(subprocess.TimeoutExpired):
        _run_tolerating_teardown_wedge(
            [sys.executable, str(script)], cwd=tmp_path, env=dict(os.environ), timeout=5
        )
