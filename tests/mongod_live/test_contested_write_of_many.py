"""A write of many whose plan mongod chooses by trial (RM-390).

mongod tries a contested updateMany's or deleteMany's candidate plans over
what their scans reach, through its plan cache, then writes each document
the winner returns. Since 2026-11-13 the Rust server does the same: the
trial runs over what the candidates reach (`explain::order::tried_write`)
and the winner streams into the write, where before it read every match
first to choose. These pin what that choice leaves behind against mongod
8.2: the plan cache's entry over three writes of one shape (inactive,
its works raised, then active), the order a deleteMany's batches name the
documents in, and what the statement reports it examined.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import same

DB = "live_contested_write"
#: Past the 2,000 documents a trial reads whole: it reads what each
#: candidate's scan reaches.
DOCS = 5000

QUERIES = [
    # Both indexes return everything: the first to 101 results wins.
    {"a": {"$gte": 0}, "b": {"$gte": 0}},
    # A point on `b` against a range on `a`.
    {"a": {"$lt": 300}, "b": 3},
    # A narrow range on `a` against a wide one on `b`.
    {"a": {"$gte": 4990}, "b": {"$lte": 6}},
]


def _fill(db: Any) -> None:
    db.drop_collection("c")
    db.c.insert_many([{"_id": i, "a": i, "b": i % 7, "n": 0} for i in range(DOCS)])
    db.c.create_index("a")
    db.c.create_index("b")


def _cache(db: Any) -> list:
    return sorted(
        (e.get("isActive"), e.get("works"), str(e.get("cachedPlan")))
        for e in db.c.aggregate([{"$planCacheStats": {}}])
    )


def _writes(client: Any, op: str, query: dict) -> dict:
    db = client[DB]
    _fill(db)
    out: dict = {}
    for round_ in range(3):
        if op == "update":
            r = db.command(
                {"update": "c", "updates": [{"q": query, "u": {"$inc": {"n": 1}}, "multi": True}]}
            )
            out[f"written{round_}"] = (r["n"], r["nModified"])
        else:
            if round_:
                db.c.insert_many(
                    [
                        {"_id": DOCS + 1000 * round_ + i, "a": i, "b": i % 7, "n": 0}
                        for i in range(30)
                    ]
                )
            r = db.command({"delete": "c", "deletes": [{"q": query, "limit": 0}]})
            out[f"written{round_}"] = r["n"]
        out[f"cache{round_}"] = _cache(db)
    if op == "delete":
        # The last delete's batches, newest first, as its oplog names them.
        entries = client["local"]["oplog.rs"].find(
            {"op": "c", "ns": "admin.$cmd", "o.applyOps.ns": f"{DB}.c"}
        )
        batches = list(entries.sort("$natural", -1).limit(3))
        out["batches"] = [[o["o"]["_id"] for o in e["o"]["applyOps"]] for e in batches]
        assert out["batches"], "no batched delete in the oplog"
    return out


def _profiled(client: Any, op: str, query: dict) -> dict:
    db = client[DB]
    _fill(db)
    db.command("profile", 0)
    db.drop_collection("system.profile")
    db.command("profile", 2)
    try:
        if op == "update":
            db.command(
                {"update": "c", "updates": [{"q": query, "u": {"$inc": {"n": 1}}, "multi": True}]}
            )
        else:
            db.command({"delete": "c", "deletes": [{"q": query, "limit": 0}]})
    finally:
        db.command("profile", 0)
    entry = db["system.profile"].find_one({"op": "update" if op == "update" else "remove"})
    assert entry is not None
    out = {k: entry.get(k) for k in ("planSummary", "nMatched", "ndeleted")}
    # The counts' values; their BSON type (int32 on mongod, int64 here) is
    # every profile entry's and RM-601's gap test, not this plan's.
    for k in ("keysExamined", "docsExamined"):
        out[k] = int(entry[k])
    return out


@pytest.mark.parametrize("op", ["update", "delete"])
@pytest.mark.parametrize("query", QUERIES)
def test_a_contested_write_of_many_plans_as_mongod(
    rust_mongo: Any, mongod_rs: Any, op: str, query: dict
) -> None:
    same(_writes(rust_mongo, op, query), _writes(mongod_rs, op, query))


@pytest.mark.parametrize("op", ["update", "delete"])
@pytest.mark.parametrize("query", QUERIES)
def test_a_contested_write_of_many_reports_its_winners_reads(
    rust_mongo: Any, mongod_rs: Any, op: str, query: dict
) -> None:
    same(_profiled(rust_mongo, op, query), _profiled(mongod_rs, op, query))
