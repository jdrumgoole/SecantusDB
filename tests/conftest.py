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
import types

import pytest

# Make sibling test-helper modules (e.g. ``sqlfake``) importable by bare name
# regardless of pytest's rootdir/import-mode.
sys.path.insert(0, os.path.dirname(__file__))

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
