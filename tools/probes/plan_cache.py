"""Differential-probe mongod's plan cache: its key's hashes and its entries.

Two parts, each run against a real mongod and the server under test:

* SHAPES -- one `explain` per query shape (match operators, paths, sorts,
  projections, collations, distinct keys, indexes that discriminate: sparse,
  partial, collated, multikey) compared by the `queryHash`,
  `planCacheShapeHash` and `planCacheKey` they print. mongod hashes its
  `canonical_query_encoder` string with MurmurHash3; a difference names a
  shape the encoder here gets wrong.
* SCENARIOS -- a query shape run with other values, step by step, compared
  by the `_id`s each read returns, in order, and by `$planCacheStats` after
  it (hashes, `isActive`, `works`, the cached plan's index), so that an
  entry made, activated, grown, reused, replanned or cleared differently
  shows where it happens.

Not compared: `timeOfCreation`, `host`, `solutionHash` (per process in
mongod), `estimatedSizeBytes` (RM-341) and timings.

    PROBE_MONGOD="mongodb://127.0.0.1:27017/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27018/?directConnection=true" \\
        python tools/probes/plan_cache.py

`PROBE_ONLY=<substring>` runs the shapes and scenarios whose name holds it.
"""

from __future__ import annotations

import os
import re
import sys
from typing import Any

import pymongo
from bson import MaxKey, MinKey, Regex
from pymongo.errors import PyMongoError

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041/?directConnection=true")
SERVER = os.environ.get("PROBE_SERVER")
ONLY = os.environ.get("PROBE_ONLY", "")
DB = "probe_plan_cache"


def answer(fn: Any) -> Any:
    try:
        return fn()
    except PyMongoError as e:
        return ("err", getattr(e, "code", None))


def seed(client: Any, indexes: list) -> Any:
    client.drop_database(DB)
    col = client[DB].c
    col.insert_many(
        [
            {
                "_id": i,
                "a": i % 10,
                "b": i % 7,
                "s": f"x{i % 5}",
                "t": [i % 3, i % 4],
                "n": {"k": i % 6},
                "loc": [i % 10, i % 9],
            }
            for i in range(60)
        ]
    )
    for key, opts in indexes:
        col.create_index(key, **opts)
    return col


PLAIN = [("a", {}), ("b", {}), ("t", {}), ("n.k", {})]
SPECIAL = [
    ("a", {}),
    ("b", {"sparse": True}),
    ("s", {"collation": {"locale": "fr"}}),
    ("t", {"partialFilterExpression": {"a": {"$gt": 2}}}),
    ([("loc", "2d")], {}),
]

#: (name, indexes, explained command)
SHAPES: list[tuple[str, list, dict]] = []


def shape(name: str, indexes: list, **cmd: Any) -> None:
    SHAPES.append((name, indexes, {"find": "c", **cmd}))


for ix_name, ix in [("plain", PLAIN), ("special", SPECIAL)]:
    for fname, f in [
        ("eq", {"a": 1}),
        ("and", {"a": 1, "b": 3}),
        ("empty", {}),
        ("range", {"a": {"$gt": 1, "$lt": 5}}),
        ("in", {"a": {"$in": [1, 2]}}),
        ("in regex", {"s": {"$in": ["x1", re.compile("x", re.I)]}}),
        ("regex flags", {"s": {"$regex": "^x", "$options": "mi"}}),
        ("regex plain", {"s": Regex("^x")}),
        ("ne", {"a": {"$ne": 3}}),
        ("ne null", {"b": {"$ne": None}}),
        ("eq null", {"b": None}),
        ("not gt", {"a": {"$not": {"$gt": 3}}}),
        ("exists", {"b": {"$exists": True}}),
        ("not exists", {"b": {"$exists": False}}),
        ("type", {"a": {"$type": "int"}}),
        ("mod", {"a": {"$mod": [2, 0]}}),
        ("size", {"t": {"$size": 2}}),
        ("all", {"t": {"$all": [1, 2]}}),
        ("elemMatch value", {"t": {"$elemMatch": {"$gt": 1, "$lt": 5}}}),
        ("elemMatch object", {"t": {"$elemMatch": {"x": 1, "y": {"$gt": 2}}}}),
        ("or", {"$or": [{"a": 1}, {"b": 2}]}),
        ("or and", {"$or": [{"a": 1}, {"b": 2}], "s": "x1"}),
        ("nor", {"$nor": [{"a": 1}, {"b": 2}]}),
        ("gt minkey", {"a": {"$gt": MinKey()}}),
        ("lt maxkey", {"a": {"$lt": MaxKey()}}),
        ("expr", {"$expr": {"$eq": ["$a", 1]}}),
        ("bits", {"a": {"$bitsAllSet": 1}}),
        ("dotted", {"n.k": 1}),
        ("string", {"s": "x1"}),
        ("string range", {"s": {"$gt": "x1"}}),
        ("partial implied", {"t": 1, "a": {"$gt": 5}}),
        ("geo within", {"loc": {"$geoWithin": {"$box": [[0, 0], [5, 5]]}}}),
        ("geo center", {"loc": {"$geoWithin": {"$center": [[0, 0], 5]}}}),
    ]:
        shape(f"{ix_name}: {fname}", ix, filter=f)
    shape(f"{ix_name}: sort", ix, filter={"a": 1}, sort={"b": -1, "s": 1})
    shape(f"{ix_name}: projection", ix, filter={"a": 1}, projection={"b": 1})
    shape(f"{ix_name}: projection no _id", ix, filter={"a": 1}, projection={"b": 1, "_id": 0})
    shape(f"{ix_name}: projection dotted", ix, filter={"a": 1}, projection={"n.k": 1, "b": 1})
    shape(f"{ix_name}: exclusion", ix, filter={"a": 1}, projection={"b": 0})
    shape(f"{ix_name}: computed", ix, filter={"a": 1}, projection={"z": "$b"})
    shape(
        f"{ix_name}: collation",
        ix,
        filter={"s": "x1"},
        collation={"locale": "fr", "strength": 2},
    )
    shape(
        f"{ix_name}: collation every option",
        ix,
        filter={"s": "x1"},
        collation={
            "locale": "en_US",
            "caseLevel": True,
            "caseFirst": "upper",
            "numericOrdering": True,
            "alternate": "shifted",
            "maxVariable": "space",
            "backwards": True,
            "normalization": True,
        },
    )
    SHAPES.append((f"{ix_name}: distinct", ix, {"distinct": "c", "key": "b", "query": {"a": 1}}))
    SHAPES.append((f"{ix_name}: count", ix, {"count": "c", "query": {"a": 1, "b": 2}}))
    SHAPES.append(
        (
            f"{ix_name}: aggregate project",
            ix,
            {
                "aggregate": "c",
                "pipeline": [{"$match": {"a": 1}}, {"$project": {"b": 1}}],
                "cursor": {},
            },
        )
    )
    SHAPES.append(
        (
            f"{ix_name}: aggregate group (SBE)",
            ix,
            {
                "aggregate": "c",
                "pipeline": [{"$match": {"a": 1}}, {"$group": {"_id": "$b"}}],
                "cursor": {},
            },
        )
    )


def hashes(client: Any, indexes: list, cmd: dict) -> Any:
    col = seed(client, indexes)
    e = answer(lambda: col.database.command("explain", cmd, verbosity="queryPlanner"))
    if isinstance(e, tuple):
        return e
    qp = e.get("queryPlanner") or e["stages"][0]["$cursor"]["queryPlanner"]
    return {k: qp.get(k) for k in ("queryHash", "planCacheShapeHash", "planCacheKey")}


#: (name, documents, steps): a step is a filter to find, or a callable.
SCENARIOS: list[tuple[str, int, list]] = [
    (
        "values that would pick another plan",
        2000,
        [
            {"a": {"$lt": x}, "b": {"$lt": y}}
            for x, y in [(5, 90), (30, 20), (90, 5), (50, 50), (20, 30), (3, 60), (60, 3), (40, 40)]
        ],
    ),
    (
        "index built, hidden, dropped",
        1500,
        [
            {"a": {"$lt": 5}, "b": {"$lt": 90}},
            {"a": {"$lt": 5}, "b": {"$lt": 90}},
            lambda col: col.create_index("c"),
            {"a": {"$lt": 5}, "b": {"$lt": 90}},
            lambda col: col.database.command("collMod", "c", index={"name": "c_1", "hidden": True}),
            {"a": {"$lt": 5}, "b": {"$lt": 90}},
            {"a": {"$lt": 5}, "b": {"$lt": 90}},
            lambda col: col.drop_index("c_1"),
            {"a": {"$lt": 5}, "b": {"$lt": 90}},
            lambda col: col.insert_one({"_id": -1, "a": [1, 2], "b": 5}),
            {"a": {"$lt": 5}, "b": {"$lt": 90}},
            {"a": {"$lt": 5}, "b": {"$lt": 90}},
            lambda col: col.database.command("planCacheClear", "c"),
            {"a": {"$lt": 5}, "b": {"$lt": 90}},
        ],
    ),
    (
        "rooted $or branches",
        2000,
        [
            {"$or": [{"a": 1, "b": 2}, {"a": {"$lt": 3}, "b": {"$gt": 4}}]},
            {"$or": [{"a": 1, "b": 2}, {"a": {"$lt": 3}, "b": {"$gt": 4}}]},
            {"a": {"$lt": 5}, "b": {"$gt": 90}},
        ],
    ),
]


def scenario(client: Any, n: int, steps: list) -> list:
    client.drop_database(DB)
    col = client[DB].c
    col.insert_many([{"_id": i, "a": i % 100, "b": (i // 20) % 100} for i in range(n)])
    col.create_index("a")
    col.create_index("b")
    out = []
    for step in steps:
        if callable(step):
            got = answer(lambda step=step: step(col))
            out.append(got if isinstance(got, tuple) else "ok")
        else:
            out.append([d["_id"] for d in col.find(step)])
        entries = answer(lambda: list(col.aggregate([{"$planCacheStats": {}}])))
        if not isinstance(entries, tuple):
            entries = [
                (
                    e["planCacheShapeHash"],
                    e["planCacheKey"],
                    e["isActive"],
                    e["works"],
                    e["cachedPlan"].get("inputStage", {}).get("indexName"),
                )
                for e in entries
            ]
        out.append(entries)
    return out


def main() -> int:
    if not SERVER:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    mongod = pymongo.MongoClient(MONGOD)
    server = pymongo.MongoClient(SERVER)
    differ = 0
    shapes = [s for s in SHAPES if ONLY in s[0]]
    for name, indexes, cmd in shapes:
        want, got = hashes(mongod, indexes, cmd), hashes(server, indexes, cmd)
        if want != got:
            differ += 1
            print(f"--- shape {name}\n    mongod: {want}\n    server: {got}")
    scenarios = [s for s in SCENARIOS if ONLY in s[0]]
    steps = 0
    for name, n, plan in scenarios:
        want, got = scenario(mongod, n, plan), scenario(server, n, plan)
        steps += len(want)
        bad = [i for i, (w, g) in enumerate(zip(want, got, strict=False)) if w != g]
        if bad or len(want) != len(got):
            differ += len(bad) or 1
            print(f"--- scenario {name} ({len(bad)} of {len(want)} replies)")
            for i in bad[:5]:
                print(f"  reply {i}")
                print(f"    mongod: {str(want[i])[:400]}")
                print(f"    server: {str(got[i])[:400]}")
    print(
        f"\n=== plan cache: {len(shapes)} shapes, {len(scenarios)} scenarios "
        f"({steps} replies) -- {differ} differ ==="
    )
    mongod.drop_database(DB)
    server.drop_database(DB)
    return 1 if differ else 0


if __name__ == "__main__":
    sys.exit(main())
