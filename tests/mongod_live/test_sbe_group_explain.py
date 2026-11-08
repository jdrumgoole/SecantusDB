"""`explain` of a `$group` mongod pushes into its slot-based engine (RM-071).

mongod 8.2.11 runs a `$group` behind a collection scan in SBE and explains
it with `explainVersion` "2": a `winningPlan` of `{queryPlan, slotBasedPlan}`
whose `stages` text is the slot program, and `executionStats` made of the
SBE stages (`scan`, `project`, `group`) that ran it. The whole reply is
compared, slot program text included, with each server's collection UUID
replaced by a placeholder; timings are not compared.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import same

DB = "live_sbe_group"
VOLATILE = {
    "executionTimeMillisEstimate",
    "executionTimeMillis",
    "optimizationTimeMillis",
    "queryShapeHash",
    "serverInfo",
    "serverParameters",
    "command",
    "$clusterTime",
    "operationTime",
}

DOCS = [{"a": i % 3, "b": i, "c": {"d": i, "e": [i, {"f": i}]}} for i in range(10)]

PIPELINES = [
    [{"$group": {"_id": "$a"}}],
    [{"$group": {"_id": None}}],
    [{"$group": {"_id": "x"}}],
    [{"$group": {"_id": {"$literal": 5}}}],
    [{"$group": {"_id": None, "n": {"$sum": 1}}}],
    [{"$group": {"_id": "$a", "n": {"$sum": "$b"}}}],
    [{"$group": {"_id": "$b", "n": {"$sum": "$a"}}}],
    [{"$group": {"_id": "$a", "n": {"$sum": 2}, "m": {"$sum": 1.5}}}],
    [
        {
            "$group": {
                "_id": "$a",
                "n": {"$avg": "$b"},
                "f": {"$first": "$b"},
                "m": {"$max": "$b"},
                "p": {"$push": "$b"},
            }
        }
    ],
    [
        {
            "$group": {
                "_id": "$a",
                "l": {"$last": "$b"},
                "mi": {"$min": "$b"},
                "s": {"$addToSet": "$b"},
                "k": {"$count": {}},
            }
        }
    ],
    [{"$group": {"_id": "$c.d"}}],
    [{"$group": {"_id": "$a", "n": {"$avg": "$c.d"}}}],
    [{"$group": {"_id": "$a", "n": {"$sum": "$$ROOT"}}}],
    [{"$group": {"_id": "$$ROOT"}}],
    [{"$group": {"_id": "$b"}}, {"$limit": 3}],
    [{"$group": {"_id": "$a"}}, {"$project": {"x": "$_id"}}],
    [{"$group": {"_id": {"x": "$a", "y": None, "z": "q"}}}],
    [{"$group": {"_id": {"x": "$a"}}}],
    [{"$group": {"_id": "$c.e.f"}}],
]


def strip(x: Any, uuid: str) -> Any:
    if isinstance(x, dict):
        return {k: strip(v, uuid) for k, v in x.items() if k not in VOLATILE}
    if isinstance(x, list):
        return [strip(v, uuid) for v in x]
    if isinstance(x, str):
        return x.replace(uuid, "<uuid>")
    return x


def _explain(client: Any, coll: str, docs: list, pipeline: list, verbosity: str) -> Any:
    db = client[DB]
    if coll not in db.list_collection_names():
        if docs:
            db[coll].insert_many([dict(d) for d in docs])
        else:
            db.create_collection(coll)
    raw = next(c for c in db.list_collections() if c["name"] == coll)["info"]["uuid"]
    uuid = str(raw.as_uuid() if hasattr(raw, "as_uuid") else raw)
    cmd = {"aggregate": coll, "pipeline": pipeline, "cursor": {}}
    # Warm: the reply is compared from a second run.
    db.command("explain", cmd, verbosity=verbosity)
    return strip(db.command("explain", cmd, verbosity=verbosity), uuid)


@pytest.mark.parametrize("verbosity", ["queryPlanner", "executionStats", "allPlansExecution"])
@pytest.mark.parametrize("pipeline", PIPELINES, ids=[str(p) for p in PIPELINES])
def test_group_explain_matches_mongod(
    rust_mongo: Any, mongod: Any, pipeline: list, verbosity: str
) -> None:
    same(
        _explain(rust_mongo, "c", DOCS, pipeline, verbosity),
        _explain(mongod, "c", DOCS, pipeline, verbosity),
    )


@pytest.mark.parametrize("n", [0, 31, 32, 33, 96, 300])
@pytest.mark.parametrize("tail", [{"$project": {"x": "$_id"}}, {"$limit": 40}])
def test_cursor_batches_match_mongod(rust_mongo: Any, mongod: Any, n: int, tail: dict) -> None:
    """The `$cursor` loads 32 documents, then twice as many each batch:
    its stages' `saveState` / `restoreState` and `nReturned` count them."""
    coll = f"n{n}"
    docs = [{"b": i} for i in range(n)]
    pipeline = [{"$group": {"_id": "$b"}}, tail]
    same(
        _explain(rust_mongo, coll, docs, pipeline, "executionStats"),
        _explain(mongod, coll, docs, pipeline, "executionStats"),
    )


def test_a_sort_after_the_group_is_listed_after_the_cursor(rust_mongo: Any, mongod: Any) -> None:
    """The plan under a `$cursor`, the `$sort` after it (its byte estimate
    is RM-435's)."""
    pipeline = [{"$group": {"_id": "$a"}}, {"$sort": {"_id": 1}}]
    same(
        _explain(rust_mongo, "c", DOCS, pipeline, "queryPlanner"),
        _explain(mongod, "c", DOCS, pipeline, "queryPlanner"),
    )


@pytest.mark.parametrize(
    "pipeline",
    [
        [{"$group": {"_id": "$a"}}],
        [{"$group": {"_id": "$a", "n": {"$stdDevPop": "$b"}}}],
        [{"$lookup": {"from": "m", "localField": "a", "foreignField": "a", "as": "j"}}],
    ],
)
@pytest.mark.parametrize("verbosity", ["queryPlanner", "executionStats"])
def test_a_collection_that_does_not_exist(
    rust_mongo: Any, mongod: Any, pipeline: list, verbosity: str
) -> None:
    """Over no collection any pushed-down pipeline is `limit 0` over `coscan`."""
    cmd = {"aggregate": "nope", "pipeline": pipeline, "cursor": {}}
    same(
        strip(rust_mongo[DB].command("explain", cmd, verbosity=verbosity), "-"),
        strip(mongod[DB].command("explain", cmd, verbosity=verbosity), "-"),
    )
