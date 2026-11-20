"""Record the oplog entries of a PREPARED transaction on mongod 8.2.11.

Phase 4b-2 of ``tasks/mongod-storage-model-plan.md``. `prepareTransaction` is
refused unless the node is a shard server, so this starts a config server
replica set and a `--shardsvr` one whose `admin.system.version` holds a
`shardIdentity` pointing at it, then runs a transaction through
`prepareTransaction` and `commitTransaction` / `abortTransaction`.

Usage: .venv/bin/python tools/probes/mongod_oplog_prepare.py --out FILE
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
from bson import ObjectId, Timestamp
from pymongo import MongoClient

MONGOD = "/opt/homebrew/Cellar/mongodb-community@8.2/8.2.11/bin/mongod"


def _port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_listening(port: int) -> None:
    for _ in range(200):
        with contextlib.suppress(OSError):
            socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
            return
        time.sleep(0.2)
    raise SystemExit(f"mongod on {port} did not start")


def _clean(v):
    if isinstance(v, Timestamp):
        return ["ts", v.time, v.inc]
    if isinstance(v, bson.Binary):
        return f"<bin{v.subtype}:{len(v)}>"
    if isinstance(v, dict):
        return {k: _clean(x) for k, x in v.items()}
    if isinstance(v, list):
        return [_clean(x) for x in v]
    return v


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    scratch = Path(__file__).resolve().parents[2] / ".scratch"
    scratch.mkdir(exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="prep-", dir=scratch))
    procs: list[subprocess.Popen] = []

    def start(name: str, port: int, *extra: str) -> subprocess.Popen:
        (work / name).mkdir(exist_ok=True)
        proc = subprocess.Popen(
            [
                MONGOD,
                "--dbpath",
                str(work / name),
                "--port",
                str(port),
                "--bind_ip",
                "127.0.0.1",
                "--logpath",
                str(work / f"{name}.log"),
                "--logappend",
                *extra,
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        procs.append(proc)
        _wait_listening(port)
        return proc

    try:
        cfg_port, port = _port(), _port()
        start("cfg", cfg_port, "--replSet", "cfg", "--configsvr")
        cc = MongoClient(f"mongodb://127.0.0.1:{cfg_port}/?directConnection=true")
        cc.admin.command(
            "replSetInitiate",
            {
                "_id": "cfg",
                "configsvr": True,
                "members": [{"_id": 0, "host": f"127.0.0.1:{cfg_port}"}],
            },
        )
        while not cc.admin.command("hello").get("isWritablePrimary"):
            time.sleep(0.2)
        shard = start("shard", port, "--replSet", "rs0")
        c = MongoClient(f"mongodb://127.0.0.1:{port}/?directConnection=true")
        c.admin.command(
            "replSetInitiate", {"_id": "rs0", "members": [{"_id": 0, "host": f"127.0.0.1:{port}"}]}
        )
        while not c.admin.command("hello").get("isWritablePrimary"):
            time.sleep(0.2)
        c.admin.system.version.insert_one(
            {
                "_id": "shardIdentity",
                "shardName": "rs0",
                "clusterId": ObjectId(),
                "configsvrConnectionString": f"cfg/127.0.0.1:{cfg_port}",
            }
        )
        with contextlib.suppress(Exception):
            c.admin.command("shutdown")
        shard.wait(60)
        start("shard", port, "--replSet", "rs0", "--shardsvr")
        c = MongoClient(f"mongodb://127.0.0.1:{port}/?directConnection=true")
        for _ in range(100):
            if c.admin.command("hello").get("isWritablePrimary"):
                break
            time.sleep(0.3)
        db = c.app
        db.create_collection("c")
        oplog = c.local["oplog.rs"]
        out = []

        def run(name: str, fn) -> None:
            last = next(oplog.find().sort("$natural", -1).limit(1))["ts"]
            err = None
            try:
                fn()
            except Exception as exc:  # recorded, not hidden
                err = str(exc)[:300]
            time.sleep(0.5)
            es = [
                e
                for e in oplog.find({"ts": {"$gt": last}}).sort("$natural", 1)
                if not (e.get("op") == "n" and e.get("o", {}).get("msg") == "periodic noop")
            ]
            out.append(
                {
                    "name": name,
                    "error": err,
                    "entries": [_clean({k: v for k, v in e.items() if k != "wall"}) for e in es],
                }
            )

        def prepared(commit: bool):
            def f() -> None:
                s = c.start_session()
                s.start_transaction()
                db.c.insert_many([{"_id": i} for i in range(3)], session=s)
                db.c.update_one({"_id": 0}, {"$set": {"a": 1}}, session=s)
                ts = c.admin.command("prepareTransaction", session=s)["prepareTimestamp"]
                if commit:
                    c.admin.command({"commitTransaction": 1, "commitTimestamp": ts}, session=s)
                else:
                    c.admin.command("abortTransaction", session=s)

            return f

        run("prepare + commit", prepared(True))
        db.c.delete_many({})
        run("prepare + abort", prepared(False))
        Path(args.out).write_text(
            json.dumps({"mongod": "8.2.11", "steps": out}, indent=1, default=str)
        )
        with contextlib.suppress(Exception):
            c.admin.command("shutdown")
    finally:
        for p in procs:
            if p.poll() is None:
                p.kill()
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
