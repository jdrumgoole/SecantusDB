"""Concurrent WiredTiger stress harness for the Rust server.

Reproduces the cross-server PITR load that made WiredTiger's eviction thread
panic under the parallel test suite: many threads each spin up an embedded Rust
server (its own WT connection), write CRUD history, stop it, reopen the store
in the same process (a *second* WT connection over the same data dir) and
check what it holds, rebuild it with ``secantusd-mdb restore``, and open the
restored store. Run it with enough workers/iterations to exceed what
`pytest -n auto` applies, and assert the process survives -- no `WT_PANIC` --
and every store reads back what was written.

The restore used to be the deprecated Python restore tool, a Python store
opening the Rust server's. Since the Rust servers write mongod's layout
(tasks/mongod-storage-model-plan.md) the Python store cannot read theirs, so
the second connection is a Rust one in this process and the restore is the
Rust binary's.

Usage (via invoke): ``inv rust-stress --workers 16 --iters 5``
"""

from __future__ import annotations

import argparse
import concurrent.futures
import os
import subprocess
import sys
import tempfile
import traceback
from pathlib import Path

import _secantus_server as _server
import pymongo

_REPO = Path(__file__).resolve().parent.parent


def _binary() -> Path:
    """The `secantusd-mdb` this checkout built (release first)."""
    env = os.environ.get("SECANTUSDB_BIN")
    if env:
        return Path(env)
    for profile in ("release", "debug"):
        p = _REPO / "crates" / "secantusdb" / "target" / profile / "secantusd-mdb"
        if sys.platform == "win32":
            p = p.with_suffix(".exe")
        if p.exists():
            return p
    raise SystemExit("secantusd-mdb is not built (./inv rust-binary-build)")


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
        # A second WiredTiger connection over the same data dir, in this
        # process: what the stress is for.
        _expect(data, 150)
        out = Path(td) / "restored"
        done = subprocess.run(
            [str(_binary()), "restore", "--source", str(data), "--target-dir", str(out)],
            capture_output=True,
            text=True,
            timeout=600,
        )
        if done.returncode != 0:
            raise RuntimeError(f"restore exited {done.returncode}: {done.stderr}")
        _expect(out, 150)


def _expect(path: Path, count: int) -> None:
    """Open the store at `path` in this process and check it holds `count`
    documents in `app.c`, each touched."""
    srv = _server.RustServer(str(path), 0)
    try:
        coll = _client(srv)["app"]["c"]
        got = coll.count_documents({})
        touched = coll.count_documents({"touched": True})
        if (got, touched) != (count, count):
            raise AssertionError(f"{path}: {got} documents, {touched} touched; want {count}")
    finally:
        srv.stop()


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
