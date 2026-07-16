"""Server shutdown drains in-flight connection threads before closing WT.

Regression for the intermittent native crash (pytest-xdist worker death near
the end of the full suite): a per-connection daemon thread that was mid-
WiredTiger-operation when ``storage.close()`` freed the WT connection caused a
use-after-free. ``SecantusDBServer.stop()`` now closes the connection sockets,
wakes any tailable ``getMore`` blocked on the oplog condition variable, and
waits for the active-connection count to reach zero before tearing down WT.
"""

from __future__ import annotations

import contextlib
import threading
import time

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer


def test_stop_drains_in_flight_change_stream_before_close(tmp_path) -> None:
    """A connection thread parked in a change-stream ``getMore`` is drained by
    ``stop()`` — woken via the shutdown signal and reaped — so it never touches
    WiredTiger after close. ``stop()`` returns promptly (it doesn't block on the
    cursor's full ``maxAwaitTimeMS``) and leaves no active connections."""
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path))
    srv.start()
    mc = MongoClient(srv.uri, serverSelectionTimeoutMS=3000)
    cs = None
    watcher = None
    try:
        coll = mc["db"]["c"]
        coll.insert_one({"_id": 1})
        # A long await time parks the server-side getMore on the oplog CV.
        cs = coll.watch(max_await_time_ms=10_000)

        def drive_getmores() -> None:
            # Errors here are the *expected* teardown path: stop() tears the
            # connection down under the first in-flight try_next (that unblock
            # is what the test proves), and the second try_next races the
            # finally block's mc.close() — pymongo raising InvalidOperation /
            # network errors in this thread is not a failure. Left unhandled it
            # surfaces as a PytestUnhandledThreadExceptionWarning.
            with contextlib.suppress(Exception):
                for _ in range(2):
                    cs.try_next()

        watcher = threading.Thread(target=drive_getmores, daemon=True)
        watcher.start()
        time.sleep(0.3)  # let the getMore enter the oplog-CV wait
        assert srv._active_conns >= 1, "expected an in-flight connection thread"

        t0 = time.monotonic()
        srv.stop()
        elapsed = time.monotonic() - t0

        # The parked getMore was signalled (not waited out), so stop is prompt,
        # and every handler thread has exited before WT closed.
        assert srv._active_conns == 0
        assert elapsed < 5.0, f"stop() took {elapsed:.1f}s — drain didn't signal the waiter"
    finally:
        # Reap the client-side watcher thread. Closing the change stream + client
        # unblocks its in-flight ``try_next`` (a getMore ``recv`` that, on
        # Windows, does *not* return when only the server closes the socket) so
        # the thread exits instead of lingering into xdist worker shutdown. A
        # leaked thread stuck in that recv kept the worker alive until the 25-min
        # faulthandler watchdog killed it — reported as a "crash" against
        # whatever unlucky test ran next. join()+assert makes a regression loud.
        with contextlib.suppress(Exception):
            if cs is not None:
                cs.close()
        with contextlib.suppress(Exception):
            mc.close()
        if watcher is not None:
            watcher.join(timeout=15)
            assert not watcher.is_alive(), "change-stream watcher thread failed to reap"


@pytest.mark.slow
@pytest.mark.filterwarnings(
    # Stopping the server under the client mid-flight makes pymongo's own
    # background monitor thread raise as it loses the connection — benign
    # driver-side noise, not a server fault.
    "ignore::pytest.PytestUnhandledThreadExceptionWarning"
)
def test_rapid_teardown_under_read_load_drains_cleanly(tmp_path) -> None:
    # `slow`: ~43s in isolation (12 teardown iterations x 3 hammer threads).
    # Excluded from the default inner-loop suite via `-m 'not slow'`; the CI
    # `slow` lane (.github/workflows/test.yml) runs it every push so the
    # xdist-worker-crash race it guards stays covered. Do NOT cut the
    # iteration count to speed it up — intermittent races need the reps.
    """Stress: repeatedly tear a server down while connection threads hammer it
    with reads. Each ``stop()`` must drain to zero active connections without
    hanging or a use-after-close — the mechanism behind the xdist worker crash.
    A regression here either hangs (drain timeout) or crashes the worker."""
    for i in range(12):
        srv = SecantusDBServer(port=0, storage_path=str(tmp_path / f"s{i}"))
        srv.start()
        mc = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        coll = mc["db"]["c"]
        coll.insert_many([{"_id": j, "x": j} for j in range(30)])

        stop_flag = threading.Event()

        def hammer(c=coll, flag=stop_flag) -> None:
            while not flag.is_set():
                try:
                    list(c.find({"x": {"$gte": 0}}))
                except Exception:
                    return

        threads = [threading.Thread(target=hammer, daemon=True) for _ in range(3)]
        for t in threads:
            t.start()
        time.sleep(0.05)  # ensure threads are actively in WT reads

        t0 = time.monotonic()
        srv.stop()  # must drain in-flight handler threads before WT close
        assert time.monotonic() - t0 < 5.0, "stop() hit the drain timeout — a thread didn't exit"
        assert srv._active_conns == 0

        stop_flag.set()
        for t in threads:
            t.join(timeout=1.0)
        with contextlib.suppress(Exception):
            mc.close()


def test_stuck_conn_stacks_names_threads() -> None:
    """The stop-drain timeout diagnostic renders each still-live connection
    thread by its ``secantus-conn-*`` name (so a shutdown wedge names itself)."""
    from secantus.server import SecantusDBServer

    stop = threading.Event()
    t = threading.Thread(target=stop.wait, name="secantus-conn-1.2.3.4:5678", daemon=True)
    t.start()
    try:
        time.sleep(0.02)  # let it enter wait()
        dump = SecantusDBServer._format_stuck_conn_stacks()
        assert "secantus-conn-1.2.3.4:5678" in dump
    finally:
        stop.set()
        t.join(timeout=1.0)
