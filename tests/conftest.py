"""Shared pytest setup.

When the WiredTiger extension isn't built — e.g. a network-restricted dev box
where the ``vendor/wiredtiger`` submodule can't be fetched — stand in a stub
``wiredtiger`` module so the **pure-Python** parts of ``secantus`` (the operator
engines, the SQL layer) still import. ``secantus``'s package ``__init__`` pulls
in the WiredTiger-backed server, so without this any ``import secantus.*`` fails
at collection time.

This is strictly a no-op when a real WiredTiger is present (the normal case, and
always the case in CI): the stub is installed only when the module genuinely
can't be found, so a real build is never shadowed. Tests that exercise the real
``Storage`` require the real extension and are unaffected.
"""

from __future__ import annotations

import faulthandler
import importlib.machinery
import importlib.util
import os
import sys
import threading
import time
import types

import pytest

# Make sibling test-helper modules importable by bare name regardless of
# pytest's rootdir/import-mode.
sys.path.insert(0, os.path.dirname(__file__))


def _patch_xdist_loadscope_worker_restart() -> None:
    """Work around a pytest-xdist loadgroup/loadscope crash-recovery bug.

    Under ``--dist=loadgroup`` (which we use for the driver-smoke / crash-watchdog
    ``xdist_group`` markers), when a worker dies and xdist spins up a replacement,
    the loadscope scheduler can call ``_reschedule`` on the new worker *before* it
    has registered its collection. ``_assign_work_unit`` then does
    ``self.registered_collections[node]`` → ``KeyError`` → the whole session
    aborts with ``INTERNALERROR``. Observed on macOS as an intermittent worker
    death (SIGKILL — empty faulthandler, no OOM) turning a recoverable blip into a
    full run failure; CI on Linux runners never hit it.

    Guard ``_reschedule`` so a not-yet-collected node is skipped until it
    registers; ``remove_node`` has already re-queued the downed worker's tests, so
    they run on the replacement once it collects. A pure no-op unless a worker
    actually crashes. Still needed as of pytest-xdist 3.8.0 (the latest).
    """
    try:
        from xdist.scheduler.loadscope import LoadScopeScheduling
    except Exception:
        return
    orig = LoadScopeScheduling._reschedule
    if getattr(orig, "_secantus_guarded", False):
        return

    def _reschedule(self, node):  # type: ignore[no-untyped-def]
        if node not in self.registered_collections:
            return
        return orig(self, node)

    _reschedule._secantus_guarded = True  # type: ignore[attr-defined]
    LoadScopeScheduling._reschedule = _reschedule


_patch_xdist_loadscope_worker_restart()

# Fast test-mode storage (I2a): default every on-disk ``Storage`` /
# ``SecantusDBServer`` that doesn't ask otherwise to ``durable=False`` — journal
# off, no close-checkpoint. Cuts per-instance open+close ~5x and removes the
# fsync that serialises across xdist workers (the ~177 s serial floor measured
# in tasks/test-performance-plan.md). Storage still creates every table on disk,
# so schema / B-tree / within-session behaviour is exercised for real; only
# crash- / reopen-durability is dropped — which is why persistence / reopen /
# PITR / backup fixtures pass ``durable=True`` explicitly.
#
# ``SECANTUS_FORCE_DURABLE=1`` is honoured *inside* ``Storage`` and wins over
# this default, so ``SECANTUS_FORCE_DURABLE=1 <pytest>`` (and the CI durable
# lane) runs the WHOLE suite against real journal + checkpoint durability. Only
# set the fast default when force-durable is NOT requested, so an explicit
# durable run reads cleanly.
if os.environ.get("SECANTUS_FORCE_DURABLE") != "1":
    os.environ.setdefault("SECANTUS_TEST_FAST_STORAGE", "1")

if "wiredtiger" not in sys.modules and importlib.util.find_spec("wiredtiger") is None:
    _stub = types.ModuleType("wiredtiger")
    # A real ModuleSpec (rather than None) so later importlib.find_spec calls
    # against the now-present module don't raise "__spec__ is None".
    _stub.__spec__ = importlib.machinery.ModuleSpec("wiredtiger", loader=None)
    sys.modules["wiredtiger"] = _stub


# Only "all" is safe here — any policy that deletes a tmp dir *during* the
# session is banned (see pytest_configure below).
_SAFE_TMP_RETENTION = frozenset({"all"})


def _install_sigtrace() -> None:
    """SECANTUS_SIGTRACE=1: log receipt of catchable terminating signals to a
    per-pid file, then die with the default action — evidence for the xdist
    worker-death hunt (backlog: 'group-kill' theory). A worker that dies
    WITHOUT a log line was SIGKILLed (uncatchable) or crashed in C; one that
    logs SIGTERM names the signal, and phase B (SA_SIGINFO) can then chase
    the sender pid. Python-level handlers run even when the main thread is
    blocked in a syscall (PEP 475 runs handlers before retrying on EINTR).
    """
    import contextlib as _contextlib
    import signal as _signal
    import tempfile as _tempfile
    import time as _time

    trace_dir = os.environ.get("SECANTUS_SIGTRACE_DIR") or _tempfile.gettempdir()

    def _log_and_die(signo: int, frame: object) -> None:
        try:
            with open(os.path.join(trace_dir, f"sigtrace-{os.getpid()}.log"), "a") as fh:
                fh.write(
                    f"{_time.strftime('%H:%M:%S')} pid={os.getpid()} got signal "
                    f"{signo} ({_signal.Signals(signo).name}) "
                    f"test={os.environ.get('PYTEST_CURRENT_TEST', '?')}\n"
                )
        finally:
            _signal.signal(signo, _signal.SIG_DFL)
            os.kill(os.getpid(), signo)

    for signo in (
        _signal.SIGTERM,
        _signal.SIGINT,
        _signal.SIGHUP,
        _signal.SIGQUIT,
        _signal.SIGUSR1,
        _signal.SIGUSR2,
    ):
        with _contextlib.suppress(OSError, ValueError):
            _signal.signal(signo, _log_and_die)


def pytest_configure(config: pytest.Config) -> None:
    """Refuse to run under a ``tmp_path_retention_policy`` that deletes tmp
    dirs mid-session.

    This suite puts an on-disk WiredTiger database in each test's ``tmp_path``,
    and WiredTiger runs background eviction / log-server threads against that
    directory. ``tmp_path_retention_policy = "failed"`` (or ``"none"``) deletes
    a passed test's ``tmp_path`` the instant the test finishes — and if that
    test (or a fixture) left a WT connection open, the delete races those
    background threads and triggers ``WT_PANIC``. A panic poisons WiredTiger
    for the rest of that xdist worker *process*, so every later
    ``wiredtiger_open`` fails with "Device or resource busy". The race only
    fires reliably under CI's higher worker count, so it is easy to miss
    locally — a mid-session delete policy was tried to bound CI disk usage and
    reproduced exactly this (a green Windows cell, a WT_PANIC cascade on
    Linux). The right disk lever is a smaller per-instance footprint
    (``log=(prealloc=false)`` in ``Storage``), not deleting live WT homes.

    Keep the default ``"all"``. If a genuinely WT-free test set ever needs
    aggressive cleanup, run it in its own pytest invocation instead of
    flipping this policy globally.
    """
    if os.environ.get("SECANTUS_SIGTRACE") == "1":
        _install_sigtrace()
    # Start the session stall watcher (see the block below). Done here rather
    # than in a fixture because fixtures run only in xdist WORKERS, and the
    # controller is exactly the process that wedges.
    _arm_stall_watchdog(config)

    policy = config.getini("tmp_path_retention_policy")
    if policy not in _SAFE_TMP_RETENTION:
        raise pytest.UsageError(
            f"tmp_path_retention_policy={policy!r} is unsafe for this suite: "
            "deleting a passed test's tmp_path mid-session races WiredTiger's "
            "background eviction/log threads and triggers WT_PANIC (then "
            "'Device or resource busy' for the rest of the xdist worker). "
            "Only 'all' is allowed — leave it at the pytest default."
        )


# Held for the whole process: faulthandler writes to this fd from a signal
# handler, so it must never be closed / garbage-collected while the process
# lives. A module global is exactly that lifetime.
_crash_dump_file = None


def pytest_sessionstart(session: pytest.Session) -> None:
    # Arm the crash faulthandler HERE, not in pytest_configure: pytest's own
    # faulthandler plugin calls ``faulthandler.enable(file=<stderr>)`` in its
    # pytest_configure (_pytest/faulthandler.py), and hook ordering let it clobber
    # ours — the worker then dumped to (lost) stderr, not our file. sessionstart
    # runs strictly after every pytest_configure, so our file wins and stays the
    # fatal-signal target for the whole run.
    _arm_crash_faulthandler(session.config)


def _arm_crash_faulthandler(config: pytest.Config) -> None:
    """Point faulthandler's fatal-signal handler at a per-worker FILE.

    pytest already calls ``faulthandler.enable()`` — but it writes to the
    process's stderr, and under ``pytest-xdist`` a worker that hard-crashes
    (SIGSEGV / SIGABRT / SIGBUS, e.g. from a native WiredTiger or driver fault)
    dies with its stderr unflushed and unforwarded, so the traceback never
    reaches the CI log. xdist reports only "node down: Not properly terminated"
    and the culprit stays anonymous — the ws-changes change-stream crash has
    died that way ≥4× (see ``tasks/backlog.md``). Re-pointing the handler at a
    file that CI uploads as an artifact makes the next crash self-diagnose: the
    faulting thread's C/Python stack lands on disk before the process dies.

    Opt-in via ``SECANTUS_FAULTHANDLER_DIR`` (CI sets it) so local runs keep
    pytest's default stderr behaviour and drop no stray files. Runs in every
    process ``pytest_configure`` touches — the xdist controller (``workerid``
    absent → ``controller``) and each worker (``gw0`` …) get their own file, so
    a crash names the exact worker.
    """
    global _crash_dump_file
    out_dir = os.environ.get("SECANTUS_FAULTHANDLER_DIR")
    if not out_dir:
        return
    worker = getattr(config, "workerinput", {}).get("workerid", "controller")
    try:
        os.makedirs(out_dir, exist_ok=True)
        # Line-buffered so a partial write still reaches disk if the crash
        # interrupts mid-dump. faulthandler itself writes atomically per frame.
        _crash_dump_file = open(  # noqa: SIM115 — intentionally kept open process-lifetime
            os.path.join(out_dir, f"faulthandler-{worker}.log"), "w", buffering=1
        )
        faulthandler.enable(file=_crash_dump_file, all_threads=True)
    except OSError as exc:
        # A diagnostic must never break the run it is diagnosing. If the dir
        # isn't writable, fall back to pytest's stderr faulthandler.
        sys.stderr.write(f"could not arm crash faulthandler in {out_dir!r}: {exc}\n")


@pytest.fixture(scope="session", autouse=True)
def _hang_watchdog():
    """Dump every thread's stack and hard-exit if a worker is still alive 25 min
    into the session — so a wedged run names its culprit instead of dying silent.

    The per-test ``timeout`` (pytest-timeout, 600s) only covers a test's own body,
    not collection, session-scoped fixture setup, or xdist worker *shutdown*. A
    daemon/thread that never gets reaped (historically a rust-server ``stop()`` /
    change-stream-tail wedge, macOS-prone) keeps the worker process alive after
    its tests "finish", so the per-test deadline never fires and the run wedges
    until the job's ``timeout-minutes`` (or, without it, GitHub's 6-hour) kill —
    with zero diagnostics. This watchdog fires first (25 min < the 30-min job cap)
    and prints every thread's stack to stderr. Each xdist worker arms its own.
    """
    # Default 90 min, NOT the CI-tuned 25: CI pins SECANTUS_HANG_SECONDS=1500
    # explicitly (25 min < its 30-min job cap). A LOCAL run on a shared box can
    # legitimately exceed 25 min — a 0.6.0b8 release-gate run took 25:11 under a
    # parallel session's gauge load and the watchdog hard-exited five healthy
    # workers mid-suite (node down: Not properly terminated + controller
    # INTERNALERROR), with the stderr dumps lost. The watchdog exists to name a
    # WEDGED worker, not to kill a slow-but-progressing one; locally the only
    # hard deadline worth enforcing is "something is truly stuck".
    _hang_seconds = float(os.environ.get("SECANTUS_HANG_SECONDS", "5400"))
    # Route the wedge traceback to the per-worker crash FILE when one is armed
    # (SECANTUS_FAULTHANDLER_DIR): a worker that wedges in shutdown dies with its
    # stderr unflushed and unforwarded (xdist reports only "node down"), so a
    # stderr dump is lost in precisely the case this watchdog exists to explain.
    # The file survives the dead worker.
    _hang_file = _crash_dump_file if _crash_dump_file is not None else sys.stderr
    faulthandler.dump_traceback_later(_hang_seconds, file=_hang_file, exit=True)
    try:
        yield
    finally:
        # A SHUTDOWN wedge strikes AFTER this teardown would run — so cancelling
        # here disarms the watchdog in the exact window that wedges (which is why
        # the wedge has never self-dumped). When a capture file is armed, leave
        # the timer running through shutdown: on a healthy worker the process has
        # already exited before it fires, so it is a no-op; on a wedged worker it
        # is the only thing that will dump the hung stack. Default runs keep the
        # historical cancel-on-teardown behaviour.
        if _crash_dump_file is None:
            faulthandler.cancel_dump_traceback_later()


# --- Session stall detection (controller-side) ------------------------------
#
# ``_hang_watchdog`` above is a session-scoped FIXTURE, and fixtures only ever
# run inside xdist *workers*. The controller runs none, so it arms nothing.
# Nothing else covers a wedged controller either:
#   * ``--timeout`` (pytest-timeout) is PER-TEST and enforced BY THE WORKER.
#   * ``--session-timeout`` is evaluated per-item in the worker too, so with
#     nothing dispatching it never fires.
#   * ``--max-worker-restart`` restarts a node but does not bound how long the
#     controller may wait afterwards.
# That left the job-level ``timeout-minutes`` as the only backstop, costing the
# full cap (30 min Linux / 90 macOS) to learn nothing.
#
# WHY THIS ARMS UNCONDITIONALLY. The first version armed only from
# ``pytest_testnodedown``, on the theory that the wedge was crash-recovery. That
# was wrong. The wedge has since been observed in a session with NO crash at
# all: a nested run printed "3 passed in 2.52s" and then never exited, dying to
# SIGKILL 180s later. Three sightings now — CI macOS (gw0 crash), a local
# full-suite run (os._exit crash), and a local full-suite run with no crash —
# so this is an xdist SHUTDOWN problem that a crash can precede but does not
# cause. Arming only on node-down misses the no-crash variant entirely.
#
# WHY THE THRESHOLD IS DERIVED, NOT FIXED. The watcher measures time since the
# last test report, so a single legitimately long test looks identical to a
# stall while it runs. The per-test deadline is therefore the floor: pytest
# kills any test at ``--timeout``, so no healthy run can go materially longer
# than that without a report. Deriving the threshold from the configured
# timeout keeps it safe under both CI (``--timeout=120`` → 300s) and a default
# local run (ini ``timeout = 600`` → 1500s), where a fixed 300s WOULD have
# false-fired on a slow test. ``SECANTUS_STALL_SECONDS`` overrides for tests.
_STALL_FLOOR_SECONDS = 300.0
_STALL_TIMEOUT_MULTIPLIER = 2.5
# Once a worker has gone down ("node down"), the run has lost a worker and may
# crawl (2 workers grinding the whole shard) or wedge (controller waiting on a
# dead node). The idle-based stall check above does NOT catch a crawl, because
# the surviving workers keep reporting — which is exactly how the macOS `core`
# shard once ran 90 min (its dispatch-event job cap) after a worker crashed at
# ~3.5 min. Cap the time we allow past the FIRST crash so the run fails fast
# with a diagnostic instead of burning to the job cap. Generous (a healthy run
# is minutes), env-overridable for tests.
_POST_CRASH_DEADLINE_SECONDS = 1200.0

_stall_seconds = _STALL_FLOOR_SECONDS
_post_crash_deadline = _POST_CRASH_DEADLINE_SECONDS
_last_progress_at = time.monotonic()
_stall_watch_armed = False
_node_down: list[str] = []
_first_node_down_at: float | None = None


def pytest_runtest_logreport(report: pytest.TestReport) -> None:
    """Timestamp every report so the watcher can see progress.

    In the controller this fires for reports forwarded from every worker, which
    is precisely the "is anything still happening?" signal we need. It also
    fires in the workers, where updating a module global is harmless.
    """
    global _last_progress_at
    _last_progress_at = time.monotonic()


@pytest.hookimpl(optionalhook=True)
def pytest_handlecrashitem(crashitem: str, report: object, sched: object) -> None:
    """Name the test a crashed worker was running, the moment it crashes.

    The stall watchdog can kill a post-crash run before pytest's summary
    prints, and the summary is the only place xdist's "worker crashed while
    running X" failure reports would otherwise appear — three worker-death
    occurrences (2026-08-14/15) left no record of WHICH tests the dead
    workers were on. This dumps the crashed item immediately, plus a memory
    snapshot for the SIGKILL-no-.ips hypothesis (fd/RSS exhaustion).
    """
    sys.stderr.write(f"\n=== crashed worker was running: {crashitem} ===\n")
    try:
        import resource
        import subprocess

        rss_mb = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / (1024 * 1024)
        vm = subprocess.run(["vm_stat"], capture_output=True, text=True, timeout=5).stdout
        free = next((ln for ln in vm.splitlines() if "free" in ln), "")
        sys.stderr.write(f"controller maxrss={rss_mb:.0f}MB; {free.strip()}\n")
    except Exception:
        pass


@pytest.hookimpl(optionalhook=True)
def pytest_testnodedown(node: object, error: object) -> None:
    """Record a dead worker so the stall report can name it.

    ``optionalhook=True`` is load-bearing, not decoration: this hook is defined
    by pytest-xdist, and without that plugin pluggy rejects the WHOLE conftest
    with ``PluginValidationError: unknown hook 'pytest_testnodedown'``, taking
    every test down with it. The ``storage-engine`` lane smoke-tests the built
    wheel under a bare ``--with pytest`` install with no xdist and failed
    exactly that way.

    A crash alone is survivable — xdist may restart the node, which is what
    ``--max-worker-restart`` budgets for — so this does not fail the run here.
    But it DUMPS the crash reason immediately: a worker that goes down "Not
    properly terminated" has crashed (WT_PANIC / segfault / abort), and ``error``
    carries its last frames — which is the one piece of evidence a silent 90-min
    crawl never surfaces (the surviving workers keep reporting, so the stall
    watcher never trips). The ``_watch`` loop then bounds the post-crash run.
    """
    global _first_node_down_at
    if not error:
        return
    reason = f"{node!r}: {error!r}"
    _node_down.append(reason)
    if _first_node_down_at is None:
        _first_node_down_at = time.monotonic()
    # Immediate capture — always in the CI log, whether or not a stall follows.
    sys.stderr.write(
        f"\n=== xdist worker went down ({len(_node_down)} total) ===\n{reason}\n"
        "The crashed worker's own stack died with it; its `error` above carries\n"
        "the last frames. Controller thread stacks follow.\n\n"
    )
    faulthandler.dump_traceback(file=sys.stderr, all_threads=True)
    sys.stderr.flush()


def _stall_trigger(
    now: float,
    last_progress_at: float,
    stall_seconds: float,
    first_node_down_at: float | None,
    post_crash_deadline: float,
) -> str | None:
    """Pure decision for the stall watchdog — return a reason to fire, else None.

    Two independent triggers:
      1. **idle stall** — no test report in ``stall_seconds`` (a wedge where no
         surviving worker makes progress; the controller sits waiting).
      2. **post-crash overrun** — a worker went down and the run is STILL going
         ``post_crash_deadline`` later. The idle check can't see this: after a
         crash the surviving workers keep reporting, so ``last_progress_at``
         keeps advancing and idle stays low — which is exactly how a crashed
         macOS shard once crawled to its 90-min job cap unnoticed.
    """
    if first_node_down_at is not None:
        since_crash = now - first_node_down_at
        if since_crash > post_crash_deadline:
            return (
                f"post-crash overrun: a worker went down {since_crash:.0f}s ago "
                f"(limit {post_crash_deadline:.0f}s) and the run is still going"
            )
    idle = now - last_progress_at
    if idle >= stall_seconds:
        return f"no test report in {idle:.0f}s (limit {stall_seconds:.0f}s)"
    return None


def _arm_stall_watchdog(config: pytest.Config) -> None:
    """Start the stall watcher unless this process is an xdist worker.

    Workers already carry ``_hang_watchdog``; this covers the controller (and a
    plain serial run, where there is no worker at all).
    """
    global _stall_watch_armed, _stall_seconds, _post_crash_deadline
    if _stall_watch_armed or hasattr(config, "workerinput"):
        return
    _stall_watch_armed = True

    pc_override = os.environ.get("SECANTUS_POST_CRASH_SECONDS")
    if pc_override:
        _post_crash_deadline = float(pc_override)

    override = os.environ.get("SECANTUS_STALL_SECONDS")
    if override:
        _stall_seconds = float(override)
    else:
        per_test = 0.0
        try:
            per_test = float(config.getoption("timeout", default=0) or 0)
        except (ValueError, TypeError):
            per_test = 0.0
        if not per_test:
            try:
                per_test = float(config.getini("timeout") or 0)
            except (ValueError, TypeError, KeyError):
                per_test = 0.0
        _stall_seconds = max(_STALL_FLOOR_SECONDS, per_test * _STALL_TIMEOUT_MULTIPLIER)

    def _watch() -> None:
        while True:
            time.sleep(5.0)
            # Deliberately does NOT stop at ``pytest_sessionfinish``. An earlier
            # version returned there, disabling the watchdog in precisely the
            # window that matters: the wedge happens during SHUTDOWN, after the
            # last report, when the run has already printed [100%] but never
            # exits. A daemon thread dies with a healthy process, so watching to
            # the very end costs nothing on a normal run.
            trigger = _stall_trigger(
                time.monotonic(),
                _last_progress_at,
                _stall_seconds,
                _first_node_down_at,
                _post_crash_deadline,
            )
            if trigger is None:
                continue
            sys.stderr.write(
                "\n=== pytest session stalled ===\n"
                f"{trigger}.\n"
                + (
                    f"workers that went down earlier: {_node_down}\n"
                    if _node_down
                    else "no worker reported going down.\n"
                )
                + "Per-test timeouts cannot fire here: they are enforced by the\n"
                "worker, and this process is the controller. Thread stacks follow.\n\n"
            )
            faulthandler.dump_traceback(file=sys.stderr, all_threads=True)
            sys.stderr.write("\n=== exiting non-zero to fail fast ===\n")
            sys.stderr.flush()
            # os._exit, not sys.exit: this is a daemon thread, and a wedged
            # controller will not process a raised exception or run atexit.
            os._exit(70)

    threading.Thread(target=_watch, name="pytest-stall-watch", daemon=True).start()
