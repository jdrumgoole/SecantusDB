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
    faulthandler.dump_traceback_later(1500, exit=True)
    try:
        yield
    finally:
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

_stall_seconds = _STALL_FLOOR_SECONDS
_last_progress_at = time.monotonic()
_stall_watch_armed = False
_node_down: list[str] = []


def pytest_runtest_logreport(report: pytest.TestReport) -> None:
    """Timestamp every report so the watcher can see progress.

    In the controller this fires for reports forwarded from every worker, which
    is precisely the "is anything still happening?" signal we need. It also
    fires in the workers, where updating a module global is harmless.
    """
    global _last_progress_at
    _last_progress_at = time.monotonic()


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
    ``--max-worker-restart`` budgets for — so this does not fail the run. It
    only annotates the diagnostics if a stall follows.
    """
    if error:
        _node_down.append(f"{node!r}: {error!r}")


def _arm_stall_watchdog(config: pytest.Config) -> None:
    """Start the stall watcher unless this process is an xdist worker.

    Workers already carry ``_hang_watchdog``; this covers the controller (and a
    plain serial run, where there is no worker at all).
    """
    global _stall_watch_armed, _stall_seconds
    if _stall_watch_armed or hasattr(config, "workerinput"):
        return
    _stall_watch_armed = True

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
            idle = time.monotonic() - _last_progress_at
            if idle < _stall_seconds:
                continue
            sys.stderr.write(
                "\n=== pytest session stalled ===\n"
                f"no test report in {idle:.0f}s (limit {_stall_seconds:.0f}s).\n"
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
