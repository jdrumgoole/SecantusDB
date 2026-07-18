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


# --- Controller-side stall detection after a worker crash -------------------
#
# ``_hang_watchdog`` above is a session-scoped FIXTURE, and fixtures only ever
# run inside xdist *workers*. The controller runs none, so it arms nothing —
# which is exactly the case that bit us: a worker died mid-session and the
# controller then waited on the dead node forever, producing 85 minutes of
# total silence before the job's ``timeout-minutes`` killed it with zero
# diagnostics (observed on the macOS lane, run 29647540645, gw0 "node down:
# Not properly terminated" at ~99%).
#
# Nothing already in the stack covers that:
#   * ``--timeout`` (pytest-timeout) is PER-TEST and enforced BY THE WORKER.
#     A dead worker enforces nothing.
#   * ``--session-timeout`` is evaluated per-item in the worker too, so with
#     nothing dispatching it never fires either.
#   * ``--max-worker-restart`` restarts the node but does not bound how long
#     the controller may then wait.
# The job-level ``timeout-minutes`` was the only working backstop, and it costs
# the full cap (30 min on Linux, 90 on the macOS cron) to learn nothing.
#
# Two deliberate design choices:
#
#   * Arm ONLY after a node actually goes down. An unconditional controller
#     timer would have to be longer than the slowest legitimate session, and
#     the durations recorder (.github/workflows/record-durations.yml) runs the
#     WHOLE suite unsharded — tens of minutes — so a blanket timer would either
#     false-fire there or be too generous to be useful here.
#   * Measure ABSENCE OF PROGRESS, not elapsed time. A crash at 10% leaves
#     minutes of legitimate work, so a fixed post-crash deadline would be a
#     false positive. The per-test deadline is 120s, so no healthy run goes
#     ``_POST_CRASH_STALL_SECONDS`` with zero completed tests.
#
# Overridable via ``SECANTUS_POST_CRASH_STALL_SECONDS`` so the behaviour can be
# exercised deterministically in a test (tests/test_crash_stall_watchdog.py)
# without sitting through the real 5-minute grace.
_POST_CRASH_STALL_SECONDS = float(os.environ.get("SECANTUS_POST_CRASH_STALL_SECONDS", "300"))

_last_progress_at = time.monotonic()
_crash_watch_armed = False


def pytest_runtest_logreport(report: pytest.TestReport) -> None:
    """Timestamp every report so the post-crash watcher can see progress.

    In the controller this fires for reports forwarded from every worker, which
    is precisely the "is anything still happening?" signal we need. It also
    fires in the workers, where updating a module global is harmless.
    """
    global _last_progress_at
    _last_progress_at = time.monotonic()


@pytest.hookimpl(optionalhook=True)
def pytest_testnodedown(node: object, error: object) -> None:
    """Controller-side hook: an xdist worker went down.

    ``optionalhook=True`` is load-bearing, not decoration. This hook is defined
    by pytest-xdist, so without that plugin installed pluggy rejects the whole
    conftest with ``PluginValidationError: unknown hook 'pytest_testnodedown'``
    — taking every test in the suite down with it, not just this watchdog. That
    is not hypothetical: the ``storage-engine`` lane smoke-tests the built wheel
    under a bare ``--with pytest`` environment with no xdist, and it failed
    exactly this way. The flag tells pluggy to skip the unknown-hook check.

    Called with a truthy ``error`` exactly where xdist prints
    ``[gwN] node down: <error>``. A crash on its own is survivable — xdist may
    restart the node and carry on, which is what ``--max-worker-restart``
    budgets for — so this does not fail the run. It only starts watching for
    the pathological follow-on where the controller then stops making progress
    entirely.
    """
    if not error:
        return

    global _crash_watch_armed
    if _crash_watch_armed:  # one watcher is enough, however many nodes die
        return
    _crash_watch_armed = True

    def _watch() -> None:
        while True:
            time.sleep(5.0)
            # NOTE: deliberately does NOT stop at ``pytest_sessionfinish``.
            # An earlier version returned there, which disabled the watchdog in
            # precisely the window that matters: a crashed worker most often
            # wedges the controller during SHUTDOWN, after the last report and
            # after sessionfinish, when the run has already printed [100%] but
            # never exits. Verified — a nested session reached [100%] and then
            # hung until SIGKILL, and this watcher had already returned.
            # A daemon thread dies with a healthy process, so watching to the
            # very end costs nothing when the run exits normally.
            idle = time.monotonic() - _last_progress_at
            if idle < _POST_CRASH_STALL_SECONDS:
                continue
            sys.stderr.write(
                "\n"
                "=== xdist controller stalled after a worker crash ===\n"
                f"node down: {node!r}: {error!r}\n"
                f"no test report in {idle:.0f}s (limit "
                f"{_POST_CRASH_STALL_SECONDS:.0f}s).\n"
                "The controller is wedged waiting on a dead worker; per-test\n"
                "timeouts cannot fire because they are enforced by the worker.\n"
                "Controller thread stacks follow.\n\n"
            )
            faulthandler.dump_traceback(file=sys.stderr, all_threads=True)
            sys.stderr.write("\n=== exiting non-zero to fail fast ===\n")
            sys.stderr.flush()
            # os._exit, not sys.exit: this is a daemon thread, and a wedged
            # controller will not process a raised exception or run atexit.
            os._exit(70)

    threading.Thread(target=_watch, name="xdist-controller-stall-watch", daemon=True).start()
