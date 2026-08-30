"""Re-verify 'probed 6.0.16' claims against mongod 8.2.1 AND SecantusDB.

Each case runs identically on both and the answers are compared, so a claim is
only 'still true' when the two agree.
"""
from __future__ import annotations

import argparse
import shutil
import socket
import subprocess
import tempfile
import time

import pymongo
from bson import Decimal128
from pymongo import MongoClient

from secantus import SecantusDBServer


def _free() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _agg(db, pipeline):
    try:
        return repr(list(db.c.aggregate(pipeline)))
    except Exception as exc:  # noqa: BLE001 - the error IS the answer
        return f"ERR {getattr(exc, 'code', None)}"


def _cmd(db, cmd):
    try:
        r = db.command(dict(cmd))
        return repr({k: v for k, v in r.items() if k not in ("$clusterTime", "operationTime")})
    except Exception as exc:  # noqa: BLE001
        return f"ERR {getattr(exc, 'code', None)}"


CASES = {
    # expressions.py
    "add-missing-is-null": lambda db: _agg(db, [{"$project": {"r": {"$add": ["$nope", 1]}}}]),
    "cmp-missing-below-null": lambda db: _agg(db, [{"$project": {"r": {"$cmp": ["$nope", None]}}}]),
    "arrayelemat-missing-null": lambda db: _agg(db, [{"$project": {"r": {"$arrayElemAt": ["$nope", 0]}}}]),
    # aggregate.py
    "stddevpop-decimal-is-double": lambda db: _agg(
        db, [{"$group": {"_id": None, "r": {"$stdDevPop": "$dec"}}}]
    ),
    "densify-non-numeric-rejected": lambda db: _agg(
        db, [{"$densify": {"field": "s", "range": {"step": 1, "bounds": "full"}}}]
    ),
    # numerics.py
    "todecimal-15-sig-digits": lambda db: _agg(db, [{"$project": {"r": {"$toDecimal": 3.0}}}]),
    # query.py
    "nan-matches-nan": lambda db: repr(sorted(d["_id"] for d in db.c.find({"nan": float("nan")}))),
    # commands.py -- bool-or-number slots
    "fam-upsert-number-ok": lambda db: _cmd(
        db, {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"z": 1}}, "upsert": 1}
    ),
    "fam-new-number-ok": lambda db: _cmd(
        db, {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"z": 1}}, "new": 1.5}
    ),
    "update-multi-strict-bool": lambda db: _cmd(
        db, {"update": "c", "updates": [{"q": {}, "u": {"$set": {"z": 1}}, "multi": 1}]}
    ),
    "fam-unknown-field": lambda db: _cmd(
        db, {"findAndModify": "c", "query": {"_id": 1}, "update": {"$set": {"z": 1}}, "zz": 1}
    ),
    "getmore-unknown-field": lambda db: _cmd(db, {"getMore": 1, "collection": "c", "zz": 1}),
    "find-filter-null-rejected": lambda db: _cmd(db, {"find": "c", "filter": None}),
    "maxtimems-wrong-type": lambda db: _cmd(db, {"find": "c", "maxTimeMS": "x"}),
    "createindexes-null": lambda db: _cmd(db, {"createIndexes": "c", "indexes": None}),
    "killcursors-missing": lambda db: _cmd(db, {"killCursors": "c"}),
    # update.py
    "inc-nonnumeric-names-doc-id": lambda db: _cmd(
        db, {"update": "c", "updates": [{"q": {"_id": 2}, "u": {"$inc": {"s": 1}}}]}
    ),
    "unknown-modifier": lambda db: _cmd(
        db, {"update": "c", "updates": [{"q": {}, "u": {"$nope": {"a": 1}}}]}
    ),
    # ordering.py -- array sort takes min asc
    "array-sort-min-ascending": lambda db: repr(
        [d["_id"] for d in db.c.find({"arr": {"$exists": True}}).sort("arr", 1)]
    ),
    "array-sort-max-descending": lambda db: repr(
        [d["_id"] for d in db.c.find({"arr": {"$exists": True}}).sort("arr", -1)]
    ),
}


def seed(db) -> None:
    db.c.drop()
    db.c.insert_many(
        [
            {"_id": 1, "a": 1, "dec": Decimal128("5"), "s": "x"},
            {"_id": 2, "a": 2, "dec": Decimal128("7"), "s": "y"},
            {"_id": 3, "nan": float("nan")},
            {"_id": 4, "arr": [1, 100]},
            {"_id": 5, "arr": [50, 60]},
        ]
    )


def run(uri: str) -> dict[str, str]:
    cli = MongoClient(uri, serverSelectionTimeoutMS=10000)
    out = {}
    try:
        for name, fn in CASES.items():
            db = cli["reprobe"]
            seed(db)
            try:
                out[name] = fn(db)
            except Exception as exc:  # noqa: BLE001
                out[name] = f"RAISED {type(exc).__name__}"
    finally:
        cli.close()
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.parse_args()

    mongod = shutil.which("mongod")
    if not mongod:
        print("no mongod on PATH")
        return 2
    tmp = tempfile.mkdtemp()
    port = _free()
    proc = subprocess.Popen(
        [mongod, "--port", str(port), "--dbpath", tmp, "--quiet"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    home = tempfile.mkdtemp()
    srv = SecantusDBServer(port=0, storage_path=home)
    srv.start()
    try:
        for _ in range(60):
            try:
                MongoClient(f"mongodb://127.0.0.1:{port}/", serverSelectionTimeoutMS=500).admin.command("ping")
                break
            except Exception:  # noqa: BLE001, PERF203
                time.sleep(0.25)
        theirs = run(f"mongodb://127.0.0.1:{port}/")
        ours = run(srv.uri)
        agree = [k for k in CASES if theirs[k] == ours[k]]
        differ = [k for k in CASES if theirs[k] != ours[k]]
        print(f"mongod {pymongo.MongoClient(f'mongodb://127.0.0.1:{port}/').admin.command('buildInfo')['version']}")
        print(f"\nAGREE  ({len(agree)}/{len(CASES)}): {', '.join(agree)}")
        print(f"\nDIFFER ({len(differ)}):")
        for k in differ:
            print(f"  {k}\n      mongod: {theirs[k][:110]}\n      ours  : {ours[k][:110]}")
    finally:
        srv.stop()
        proc.terminate()
        proc.wait(timeout=30)
        shutil.rmtree(tmp, ignore_errors=True)
        shutil.rmtree(home, ignore_errors=True)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(130) from None
