"""`explain` of a pushed-down `$group` or `$count` behind a filtered, paged
collection scan, and of `count` on a view (RM-072).

mongod 8.2.11 explains `count` on a view as the aggregate it runs -- the
view's pipeline, the count's own query, skip and limit, then `$count` --
and pushes that `$count` (a `$group` of `{$sum: 1}` and a `$project`) into
SBE behind the query: a `filter` stage of `traverseF` programs over the
scan, and `limit` / `limitskip` stages. The whole reply is compared, slot
program text included, with each server's collection UUID replaced by a
placeholder; timings are not compared.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import same
from tests.mongod_live.test_sbe_group_explain import strip

DB = "live_sbe_front"

DOCS = (
    [{"a": i, "b": i % 3, "s": str(i), "t": i % 2 == 0, "d": {"e": i}} for i in range(40)]
    + [{"a": [1, 7]}, {"a": [[5]]}, {"x": 1}, {"a": float("nan")}, {"a": "str"}]
    + [{"a": None}, {"d": [{"e": 3}, {"e": 50}]}]
)

COUNT = {"$count": "n"}

PIPELINES = [
    [COUNT],
    [{"$match": {"a": 2}}, COUNT],
    [{"$match": {"a": {"$gt": 1}}}, COUNT],
    [{"$match": {"a": {"$gte": -1}}}, COUNT],
    [{"$match": {"a": {"$lt": 1}}}, COUNT],
    [{"$match": {"a": {"$lte": 2.5}}}, COUNT],
    [{"$match": {"a": {"$gt": 1, "$lt": 30}}}, COUNT],
    [{"$match": {"a": {"$gt": 1}, "b": {"$lt": 2}, "s": {"$gt": "1"}}}, COUNT],
    [{"$match": {"s": "3"}}, COUNT],
    [{"$match": {"t": True}}, COUNT],
    [{"$match": {"d.e": {"$gt": 4}}}, COUNT],
    [{"$match": {"a": {"$gt": 1}}}, {"$limit": 2}, COUNT],
    [{"$match": {"a": {"$gt": 1}}}, {"$skip": 3}, COUNT],
    [{"$match": {"a": {"$gt": 1}}}, {"$skip": 3}, {"$limit": 5}, COUNT],
    [{"$limit": 5}, {"$skip": 1}, COUNT],
    [{"$limit": 100}, COUNT],
    [{"$match": {"a": {"$gt": 1}}}, {"$group": {"_id": "$b", "n": {"$sum": "$a"}}}],
    [{"$match": {"a": {"$gt": 1}}}, {"$group": {"_id": "$d.e"}}],
    [{"$match": {"a": {"$gt": 1}}}, {"$group": {"_id": None, "n": {"$sum": 1}}}],
    [{"$match": {"a": {"$gt": 1}}}, {"$limit": 2}, {"$group": {"_id": "$$ROOT"}}],
    [{"$match": {"a": {"$gte": 5}}}, {"$group": {"_id": None, "n": {"$sum": 1}}}, {"$limit": 1}],
]


def _uuid(db: Any, coll: str) -> str:
    raw = next(c for c in db.list_collections() if c["name"] == coll)["info"]["uuid"]
    return str(raw.as_uuid() if hasattr(raw, "as_uuid") else raw)


def _setup(client: Any) -> Any:
    db = client[DB]
    if "c" not in db.list_collection_names():
        db.c.insert_many([dict(d) for d in DOCS])
        db.command("create", "v", viewOn="c", pipeline=[{"$match": {"a": {"$gt": 1}}}])
        db.command("create", "v2", viewOn="c", pipeline=[])
        db.command("create", "vv", viewOn="v", pipeline=[{"$match": {"b": {"$lt": 2}}}])
        db.command("create", "nv", viewOn="nope", pipeline=[{"$match": {"a": 1}}])
    return db


def _explain(client: Any, cmd: dict, verbosity: str) -> Any:
    db = _setup(client)
    # Warm: the reply is compared from a second run.
    db.command("explain", cmd, verbosity=verbosity)
    return strip(db.command("explain", cmd, verbosity=verbosity), _uuid(db, "c"))


VERBOSITIES = ["queryPlanner", "executionStats", "allPlansExecution"]


@pytest.mark.parametrize("verbosity", VERBOSITIES)
@pytest.mark.parametrize("pipeline", PIPELINES, ids=[str(p) for p in PIPELINES])
def test_filtered_front_explain_matches_mongod(
    rust_mongo: Any, mongod: Any, pipeline: list, verbosity: str
) -> None:
    cmd = {"aggregate": "c", "pipeline": pipeline, "cursor": {}}
    same(_explain(rust_mongo, cmd, verbosity), _explain(mongod, cmd, verbosity))


COUNTS = [
    {"count": "v"},
    {"count": "v2"},
    {"count": "vv"},
    {"count": "v", "query": {"a": {"$lt": 30}}},
    {"count": "v", "limit": 2},
    {"count": "v", "skip": 3},
    {"count": "v", "skip": 3, "limit": 5},
    {"count": "v", "limit": 100},
    {"count": "nv"},
    {"aggregate": "v", "pipeline": [COUNT], "cursor": {}},
    {"aggregate": "nope", "pipeline": [COUNT], "cursor": {}},
]


@pytest.mark.parametrize("verbosity", VERBOSITIES)
@pytest.mark.parametrize("cmd", COUNTS, ids=[str(c) for c in COUNTS])
def test_count_on_a_view_explain_matches_mongod(
    rust_mongo: Any, mongod: Any, cmd: dict, verbosity: str
) -> None:
    """`count` on a view is the slot-based plan of the aggregate it runs;
    over a view of no collection, the EOF plan under a `$cursor`."""
    same(_explain(rust_mongo, cmd, verbosity), _explain(mongod, cmd, verbosity))
