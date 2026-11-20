"""Concurrency regression guard for the embedded Rust server's teardown.

Many embedded Rust servers started and stopped concurrently in one process used
to make WiredTiger's eviction / log threads panic: ``stop()`` returned before the
detached connection threads had released their storage refs, so the connection's
final close-checkpoint raced the data-dir reopen (``WT_PANIC: ... the system must
restart`` once ``WiredTigerHS.wt`` vanished mid-checkpoint). ``stop()`` now drains
the live connection count before the WiredTiger connection closes; this test pins
that — concurrent write/stop/reopen cycles must all succeed, no panic.

The reopen used to be the deprecated Python restore tool over the stopped
store; since the Rust servers write mongod's layout (tasks/
mongod-storage-model-plan.md) the Python store cannot read theirs, so each
cycle reopens the store with a second embedded Rust server, in this process,
and checks it holds what was written.

Kept light (a handful of cycles) so it is safe under ``pytest -n auto``; the
heavier sweep lives in ``bench/wt_stress.py`` (``invoke rust-stress``).
"""

from __future__ import annotations

import concurrent.futures
import tempfile
from pathlib import Path

import pytest

_server = pytest.importorskip("_secantus_server")
# The Python server writes without timestamps, which a store whose data
# tables are not logged (secantusd-mdb's default since RM-210) refuses once a
# key has one: the hand-off is tested on a store created with them logged.
pymongo = pytest.importorskip("pymongo")


def _cycle(idx: int) -> None:
    with tempfile.TemporaryDirectory(prefix=f"rust-stress-{idx}-") as td:
        data = Path(td) / "rustdata"
        srv = _server.RustServer(str(data), 0, data_nonlogged=False)
        try:
            host, port = srv.address
            # 30s, not 5s: `RustServer(...)` returns once the listener is
            # BOUND, so a connection made before the accept thread runs waits
            # in the kernel backlog rather than failing. What cost 5s once on
            # a loaded Windows runner (2026-09-01) was four servers opening
            # WiredTiger at once ahead of the first `hello` -- scheduling, not
            # a lost connection -- so the budget is sized for a slow box.
            client = pymongo.MongoClient(
                host, port, directConnection=True, serverSelectionTimeoutMS=30000
            )
            coll = client["app"]["c"]
            coll.insert_many([{"_id": i, "v": i} for i in range(50)])
            coll.update_many({}, {"$set": {"touched": True}})
            coll.delete_many({"_id": {"$gte": 40}})
            client.close()
        finally:
            srv.stop()  # must fully close WT before the dir is reopened/removed
        # A second WiredTiger connection over the same data dir, in this
        # process, while the other workers' servers start and stop.
        again = _server.RustServer(str(data), 0, data_nonlogged=False)
        try:
            host, port = again.address
            client = pymongo.MongoClient(
                host, port, directConnection=True, serverSelectionTimeoutMS=30000
            )
            coll = client["app"]["c"]
            assert (coll.count_documents({}), coll.count_documents({"touched": True})) == (40, 40)
            client.close()
        finally:
            again.stop()


def test_concurrent_server_lifecycle_no_panic() -> None:
    workers, iters = 4, 2
    errors: list[BaseException] = []

    def run(i: int) -> None:
        try:
            _cycle(i)
        except BaseException as exc:  # noqa: BLE001 — surface any WT panic
            errors.append(exc)

    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as ex:
        list(ex.map(run, range(workers * iters)))

    assert not errors, f"{len(errors)} concurrent cycles failed: {errors[0]!r}"
