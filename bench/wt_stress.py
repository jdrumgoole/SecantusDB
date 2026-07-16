"""Concurrent WiredTiger stress harness for the Rust server.

Reproduces the cross-server PITR load that made WiredTiger's eviction thread
panic under the parallel test suite: many threads each spin up an embedded Rust
server (its own WT connection), write CRUD history, stop it, then rebuild the
database with the Python restore tool (a *second* WT connection over the same
data dir). Run it with enough workers/iterations to exceed what `pytest -n auto`
applies, and assert the process survives — no `WT_PANIC`.

Usage (via invoke): ``inv rust-stress --workers 16 --iters 5``
"""

from __future__ import annotations

import argparse
import concurrent.futures
import tempfile
import traceback
from pathlib import Path

import pymongo

import _secantus_server as _server
from secantus import oplog_replay


def _client(srv: _server.RustServer) -> pymongo.MongoClient:
    host, port = srv.address
    return pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)


def _one_cycle(idx: int) -> None:
    """One write-stop-restore cycle in its own temp dirs."""
    with tempfile.TemporaryDirectory(prefix=f"wtstress-{idx}-") as td:
        data = Path(td) / "rustdata"
        srv = _server.RustServer(str(data), 0)
        try:
            coll = _client(srv)["app"]["c"]
            coll.insert_many([{"_id": i, "v": i, "pad": "x" * 256} for i in range(200)])
            coll.update_many({}, {"$set": {"touched": True}})
            coll.delete_many({"_id": {"$gte": 150}})
        finally:
            srv.stop()
        out = Path(td) / "restored"
        oplog_replay.restore_to_timestamp(str(data), str(out))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--workers", type=int, default=16, help="concurrent threads")
    ap.add_argument("--iters", type=int, default=5, help="cycles per worker")
    args = ap.parse_args()

    total = args.workers * args.iters
    failures: list[str] = []

    def run(i: int) -> None:
        try:
            _one_cycle(i)
        except Exception:  # noqa: BLE001 — collect every failure for the report
            failures.append(traceback.format_exc())

    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as ex:
        list(ex.map(run, range(total)))

    print(f"ran {total} cycles across {args.workers} workers; {len(failures)} failed")
    for f in failures[:5]:
        print("---\n" + f)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
