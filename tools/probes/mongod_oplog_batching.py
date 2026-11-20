"""Measure which writes mongod 8.2.11 batches into one oplog entry, the
`partialTxn` chain of a transaction over 16 MB, and prepared transactions.

Phase 4b-2 of ``tasks/mongod-storage-model-plan.md``. Prints JSON of the
entries each step wrote (ts reduced to [secs, inc]; lsid/uuid/wall elided).
Usage: .venv/bin/python tools/probes/mongod_oplog_batching.py --out FILE
"""

from __future__ import annotations

import argparse
import contextlib
import json
import shutil
import socket
import subprocess
import tempfile
import time
from pathlib import Path

import bson
from bson import Timestamp
from pymongo import MongoClient

MONGOD = "/opt/homebrew/Cellar/mongodb-community@8.2/8.2.11/bin/mongod"


def _port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def clean(v):
    if isinstance(v, Timestamp):
        return ["ts", v.time, v.inc]
    if isinstance(v, bson.Binary):
        return f"<bin{v.subtype}:{len(v)}>"
    if isinstance(v, dict):
        return {k: clean(x) for k, x in v.items()}
    if isinstance(v, list):
        return [clean(x) for x in v]
    if isinstance(v, str) and len(v) > 40:
        return f"<str:{len(v)}>"
    return v


def shape(e):
    """Field names, order, and applyOps element names -- the checkable shape."""
    out = {"fields": list(e)}
    ops = e.get("o", {}).get("applyOps") if isinstance(e.get("o"), dict) else None
    if ops is not None:
        out["o_keys"] = list(e["o"])
        out["n_ops"] = len(ops)
        out["applyOps"] = sorted({tuple(o) for o in ops})
        out["applyOps"] = [list(o) for o in out["applyOps"]]
    if "prevOpTime" in e:
        pt = e["prevOpTime"]["ts"]
        out["prevOpTime_ts"] = [pt.time, pt.inc]
        out["prevOpTime_t"] = e["prevOpTime"]["t"]
    out["ts"] = [e["ts"].time, e["ts"].inc]
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    a = ap.parse_args()
    scratch = Path(__file__).resolve().parents[2] / ".scratch"
    scratch.mkdir(exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="oplog-batching-", dir=scratch))
    (work / "db").mkdir()
    port = _port()
    proc = subprocess.Popen(
        [
            MONGOD,
            "--dbpath",
            str(work / "db"),
            "--port",
            str(port),
            "--bind_ip",
            "127.0.0.1",
            "--replSet",
            "rs0",
            "--logpath",
            str(work / "m.log"),
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(150):
            with contextlib.suppress(OSError):
                socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
                break
            time.sleep(0.2)
        boot = MongoClient(f"mongodb://127.0.0.1:{port}/?directConnection=true")
        boot.admin.command(
            "replSetInitiate", {"_id": "rs0", "members": [{"_id": 0, "host": f"127.0.0.1:{port}"}]}
        )
        while not boot.admin.command("hello").get("isWritablePrimary"):
            time.sleep(0.2)
        boot.close()
        client = MongoClient(f"mongodb://127.0.0.1:{port}/?replicaSet=rs0&retryWrites=false")
        rclient = MongoClient(f"mongodb://127.0.0.1:{port}/?replicaSet=rs0&retryWrites=true")
        db, rdb = client["app"], rclient["app"]
        oplog = client["local"]["oplog.rs"]
        db.create_collection("c")
        db.create_collection("d")
        results = []

        def step(name, fn, client_=None):
            last = next(oplog.find().sort("$natural", -1).limit(1))["ts"]
            err = None
            try:
                fn()
            except Exception as exc:  # recorded, not hidden
                err = f"{type(exc).__name__}: {str(exc)[:200]}"
            time.sleep(0.3)
            es = [
                e
                for e in oplog.find({"ts": {"$gt": last}}).sort("$natural", 1)
                if not (e.get("op") == "n" and e.get("o", {}).get("msg") == "periodic noop")
            ]
            results.append(
                {
                    "name": name,
                    "error": err,
                    "count": len(es),
                    "shapes": [shape(e) for e in es],
                    "entries": [
                        clean({k: v for k, v in e.items() if k not in ("wall",)}) for e in es
                    ]
                    if sum(len(bson.encode(e)) for e in es) < 20000
                    else "<large>",
                    "sizes": [len(bson.encode(e)) for e in es],
                }
            )

        step("insert_one", lambda: db.c.insert_one({"_id": 1}))
        step(
            "insert_many 3 (unordered default ordered)",
            lambda: db.c.insert_many([{"_id": i} for i in range(10, 13)]),
        )
        step(
            "insert_many 3 ordered=False",
            lambda: db.c.insert_many([{"_id": i} for i in range(20, 23)], ordered=False),
        )
        step("insert_many 1", lambda: db.c.insert_many([{"_id": 30}]))
        step(
            "retryable insert_many 3",
            lambda: rdb.c.insert_many([{"_id": i} for i in range(40, 43)]),
        )
        step("retryable insert_one", lambda: rdb.c.insert_one({"_id": 50}))
        step("retryable delete_one", lambda: rdb.c.delete_one({"_id": 50}))
        step("retryable update_one", lambda: rdb.c.update_one({"_id": 40}, {"$set": {"x": 1}}))
        step(
            "retryable update_many 3 (non-retryable cmd)",
            lambda: rdb.c.update_many({"_id": {"$gte": 40, "$lte": 42}}, {"$set": {"y": 1}}),
        )
        step("delete_many 3", lambda: db.c.delete_many({"_id": {"$gte": 10, "$lte": 12}}))
        step("delete_many 1 match", lambda: db.c.delete_many({"_id": 20}))
        step("retryable delete_many 2", lambda: rdb.c.delete_many({"_id": {"$in": [21, 22]}}))
        step(
            "bulk_write mixed",
            lambda: db.c.bulk_write(
                [
                    __import__("pymongo").InsertOne({"_id": 60}),
                    __import__("pymongo").UpdateOne({"_id": 60}, {"$set": {"z": 1}}),
                    __import__("pymongo").DeleteOne({"_id": 60}),
                ]
            ),
        )
        # 200 docs in one insertMany: mongod's batch size limit (internalInsertMaxBatchSize 64)
        step("insert_many 200", lambda: db.c.insert_many([{"_id": 1000 + i} for i in range(200)]))
        step("delete_many 200", lambda: db.c.delete_many({"_id": {"$gte": 1000}}))
        step(
            "insert_many 5 x 1MB",
            lambda: db.c.insert_many([{"_id": 2000 + i, "p": "x" * 1_000_000} for i in range(5)]),
        )
        step("delete_many 5 x 1MB", lambda: db.c.delete_many({"_id": {"$gte": 2000}}))

        def txn(n_ops, payload, commit=True):
            def f():
                with client.start_session() as s:
                    s.start_transaction()
                    for i in range(n_ops):
                        db.c.insert_one({"_id": 5000 + i, "p": "x" * payload}, session=s)
                    if commit:
                        s.commit_transaction()
                    else:
                        s.abort_transaction()

            return f

        step("txn 3 inserts commit", txn(3, 10))
        db.c.delete_many({"_id": {"$gte": 5000}})
        step("txn 3 inserts abort", txn(3, 10, commit=False))
        step("txn 20 x 1MB (partialTxn chain)", txn(20, 1_000_000))
        db.c.delete_many({"_id": {"$gte": 5000}})
        db.c.delete_many({"_id": {"$gte": 5000}})

        def txn_ops():
            with client.start_session() as s:
                s.start_transaction()
                db.c.insert_many([{"_id": 7000 + i} for i in range(3)], session=s)
                db.c.delete_many({"_id": {"$gte": 7000}}, session=s)
                db.c.update_many({"_id": {"$lt": 100}}, {"$set": {"u": 1}}, session=s)
                s.commit_transaction()

        step("txn insert_many+delete_many+update_many", txn_ops)
        step(
            "empty txn",
            lambda: (lambda s: (s.start_transaction(), s.commit_transaction()))(
                client.start_session()
            ),
        )

        with contextlib.suppress(Exception):
            client.admin.command("shutdown")
        proc.wait(timeout=60)
        a.out.write_text(json.dumps({"mongod": "8.2.11", "steps": results}, indent=1, default=str))
    finally:
        if proc.poll() is None:
            proc.kill()
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
