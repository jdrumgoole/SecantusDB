"""Write-namespace, sort-spec and arrayFilters validation, and write-reply shape.

The Rust MongoDB server's open items from backlog section 7.02, measured
against mongod 8.2.11 on 2026-10-05 (28 of 35 shapes differed; 0 of 84 after
the fix). Covers:

- collection names on the write commands -- `insert` / `update` / `delete` /
  `findAndModify` / `createIndexes` into `a$b`, `system.*` and a namespace over
  255 characters, which an insert used to create silently;
- sort specifications in `find` and `$sort`, top level and inside `$facet`:
  key paths, `$meta`, and the numeric direction rule (doubles truncate,
  decimals round half to even -- a decimal `-1` used to sort ASCENDING);
- `arrayFilters` shapes on `update` and `findAndModify`;
- the write reply's fields and their order on a replica set (`electionId`,
  `opTime`, `writeErrors` before `ok`).

mongod must be a single-node REPLICA SET (the write-reply fields exist only
there):

    PROBE_MONGOD="mongodb://127.0.0.1:27093/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27094/?directConnection=true" \\
        python tools/probes/write_and_sort_validation.py
"""

from __future__ import annotations

import os
import sys

import pymongo
from bson import Decimal128, Int64
from pymongo.errors import BulkWriteError

MONGOD = os.environ.get("PROBE_MONGOD")
SERVER = os.environ.get("PROBE_SERVER")


def err(e):
    d = getattr(e, "details", None) or {}
    if isinstance(e, BulkWriteError):
        we = (d.get("writeErrors") or [{}])[0]
        return ("ERR", we.get("code"), we.get("errmsg"))
    return ("ERR", d.get("code"), d.get("errmsg"))


def run(uri):
    c = pymongo.MongoClient(uri, serverSelectionTimeoutMS=8000)
    db = c.probe
    for n in db.list_collection_names():
        db.drop_collection(n)
    out = {}

    def q(name, fn):
        try:
            out[name] = fn()
        except Exception as e:
            out[name] = err(e)

    # 1. collection names on implicit creation
    for name in [
        "a$b",
        "$a",
        ".a",
        "a.",
        "a..b",
        "system.foo",
        "system.profile",
        "x" * 300,
        "a b",
        "é",
    ]:
        q(
            f"insert into {name!r:.40}",
            lambda name=name: (
                db.command({"insert": name, "documents": [{"_id": 1}]}).get("n"),
                name in db.list_collection_names(),
            ),
        )
    # 2. aggregation $sort specs
    db.s.insert_many([{"_id": i, "a": i % 3} for i in range(4)])
    for spec in [
        {},
        {"a": 0},
        {"a": 2},
        {"a": "x"},
        {"a": -1.0},
        {"a": 1.5},
        {"": 1},
        {"$a": 1},
        {"a": {}},
        {"a": {"$meta": "x"}},
        {"a.": 1},
        {"a..b": 1},
    ]:
        q(
            f"$sort {spec}",
            lambda spec=spec: [
                d["_id"]
                for d in db.command(
                    {"aggregate": "s", "pipeline": [{"$sort": spec}], "cursor": {}}
                )["cursor"]["firstBatch"]
            ],
        )
    # 2b. numeric direction values, find and $sort, and a $sort inside $facet
    for v in [
        1.5,
        0.5,
        -1.9,
        Decimal128("1.5"),
        Decimal128("1.4"),
        Decimal128("0.9"),
        Decimal128("-1"),
        float("nan"),
        Int64(-1),
    ]:
        q(
            f"find sort {v!r}",
            lambda v=v: [
                d["_id"]
                for d in db.command({"find": "s", "sort": {"a": v, "_id": 1}})["cursor"][
                    "firstBatch"
                ]
            ],
        )
        q(
            f"$sort {v!r}",
            lambda v=v: [
                d["_id"]
                for d in db.command(
                    {"aggregate": "s", "pipeline": [{"$sort": {"a": v, "_id": 1}}], "cursor": {}}
                )["cursor"]["firstBatch"]
            ],
        )
        q(
            f"$facet $sort {v!r}",
            lambda v=v: db.command(
                {
                    "aggregate": "s",
                    "pipeline": [{"$facet": {"x": [{"$sort": {"a": v, "_id": 1}}]}}],
                    "cursor": {},
                }
            )["cursor"]["firstBatch"],
        )
    for spec in [{"a": {"$meta": "randVal", "x": 1}}, {"a": {"x": 1}}, {"a.$b": 1}, {".a": 1}]:
        q(
            f"find sort {spec}",
            lambda spec=spec: [
                d["_id"] for d in db.command({"find": "s", "sort": spec})["cursor"]["firstBatch"]
            ],
        )
    # 1b. system collections and the length boundary
    for name in [
        "system.js",
        "system.views",
        "system.users",
        "system.roles",
        "system.version",
        "x" * 249,
        "x" * 250,
    ]:
        q(
            f"insert into {name!r:.30}",
            lambda name=name: db.command({"insert": name, "documents": [{"_id": 1}]}).get("n"),
        )
    for label, cmd in [
        (
            "upsert a$b",
            {"update": "a$b", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "upsert": True}]},
        ),
        ("delete a$b", {"delete": "a$b", "deletes": [{"q": {}, "limit": 0}]}),
        (
            "fam a$b",
            {"findAndModify": "a$b", "query": {}, "update": {"$set": {"a": 1}}, "upsert": True},
        ),
        (
            "createIndexes a$b",
            {"createIndexes": "a$b", "indexes": [{"key": {"a": 1}, "name": "a_1"}]},
        ),
    ]:
        q(label, lambda cmd=cmd: db.command(cmd).get("ok"))
    # 3. arrayFilters shapes
    db.af.insert_one({"_id": 1, "arr": [{"x": 1}, {"x": 2}]})
    upd = {"$set": {"arr.$[e].y": 1}}
    for af in [
        [{}],
        [{"e.x": 1}, {"e.x": 2}],
        [{"e.x": 1, "f.x": 2}],
        [{"E.x": 1}],
        [{"1e.x": 1}],
        [1],
        "notarray",
        [{"$and": [{"e.x": 1}, {"f": 1}]}],
        [{"e": {"$gt": 0}}, {"e.x": 1}],
        [{"e.x": {"$bad": 1}}],
    ]:
        q(
            f"arrayFilters {af!r:.50}",
            lambda af=af: {
                k: v
                for k, v in db.command(
                    {"update": "af", "updates": [{"q": {"_id": 1}, "u": upd, "arrayFilters": af}]}
                ).items()
                if k in ("n", "nModified", "writeErrors", "ok")
            },
        )
    for af in [[{}], [{"E.x": 1}], [1], [{"e.x": 1}, {"e.x": 2}]]:
        q(
            f"fam arrayFilters {af!r:.40}",
            lambda af=af: db.command(
                {"findAndModify": "af", "query": {"_id": 1}, "update": upd, "arrayFilters": af}
            ).get("ok"),
        )
    # 4. write reply fields
    for cmd in [
        {"insert": "w", "documents": [{"_id": 1}]},
        {"update": "w", "updates": [{"q": {"_id": 1}, "u": {"$set": {"a": 1}}}]},
        {"delete": "w", "deletes": [{"q": {"_id": 1}, "limit": 1}]},
    ]:
        q(
            f"reply keys {next(iter(cmd))}",
            lambda cmd=cmd: [(k, type(v).__name__) for k, v in db.command(cmd).items()],
        )
    q(
        "reply keys insert dup",
        lambda: [
            k
            for k in db.command(
                {"insert": "w", "documents": [{"_id": 7}, {"_id": 7}], "ordered": False}
            )
        ],
    )
    db.o.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": "x"}])
    q(
        "update err order",
        lambda: list(
            db.command(
                {
                    "update": "o",
                    "updates": [
                        {"q": {"_id": 2}, "u": {"$inc": {"a": 1}}},
                        {"q": {"_id": 1}, "u": {"$set": {"b": 1}}},
                    ],
                    "ordered": False,
                }
            )
        ),
    )
    q(
        "upsert order",
        lambda: list(
            db.command(
                {
                    "update": "o",
                    "updates": [{"q": {"_id": 9}, "u": {"$set": {"b": 1}}, "upsert": True}],
                }
            )
        ),
    )
    q(
        "reply keys fam",
        lambda: [
            k
            for k in db.command(
                {"findAndModify": "w", "query": {"_id": 7}, "update": {"$set": {"b": 1}}}
            )
        ],
    )
    return out


def main() -> int:
    if not (MONGOD and SERVER):
        print("PROBE_MONGOD and PROBE_SERVER are required (see the module docstring)")
        return 2
    want, got = run(MONGOD), run(SERVER)
    # Self-check: a run whose reference answers are wrong proves nothing.
    if want.get("insert into 'a$b'", (None, None))[1] != 73:
        print("SELF-CHECK FAILED: mongod did not refuse an insert into 'a$b' with 73")
        return 2
    if "electionId" not in want.get("reply keys insert dup", []):
        print("SELF-CHECK FAILED: mongod is not a replica set (no electionId in a write reply)")
        return 2
    bad = 0
    for k in want:
        if want[k] != got.get(k):
            bad += 1
            print(f"DIFF {k}\n   mongod={want[k]}\n   ours  ={got.get(k)}")
    print(f"=== write/sort validation: {bad} of {len(want)} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
