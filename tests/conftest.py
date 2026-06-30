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
