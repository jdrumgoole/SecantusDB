"""Concurrency regression guard for the embedded Rust server's teardown.

Many embedded Rust servers started and stopped concurrently in one process used
to make WiredTiger's eviction / log threads panic: ``stop()`` returned before the
detached connection threads had released their storage refs, so the connection's
final close-checkpoint raced the data-dir reopen (``WT_PANIC: ... the system must
restart`` once ``WiredTigerHS.wt`` vanished mid-checkpoint). ``stop()`` now drains
the live connection count before the WiredTiger connection closes; this test pins
that — concurrent write/stop/restore cycles must all succeed, no panic.

Kept light (a handful of cycles) so it is safe under ``pytest -n auto``; the
heavier sweep lives in ``bench/wt_stress.py`` (``invoke rust-stress``).
"""

from __future__ import annotations

import concurrent.futures
import tempfile
from pathlib import Path

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

from secantus import oplog_replay  # noqa: E402


def _cycle(idx: int) -> None:
    with tempfile.TemporaryDirectory(prefix=f"rust-stress-{idx}-") as td:
        data = Path(td) / "rustdata"
        srv = _server.RustServer(str(data), 0)
        try:
            host, port = srv.address
            client = pymongo.MongoClient(
                host, port, directConnection=True, serverSelectionTimeoutMS=5000
            )
            coll = client["app"]["c"]
            coll.insert_many([{"_id": i, "v": i} for i in range(50)])
            coll.update_many({}, {"$set": {"touched": True}})
            coll.delete_many({"_id": {"$gte": 40}})
            client.close()
        finally:
            srv.stop()  # must fully close WT before the dir is reopened/removed
        out = Path(td) / "restored"
        oplog_replay.restore_to_timestamp(str(data), str(out))


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
