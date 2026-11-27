"""`explain` of a `$count` (or a `$group` that reads no field) over a
`FETCH` with a filter of its own over an index scan, and of `count` on a
view so served (RM-695).

mongod 8.2.11 runs the plan in SBE: a `queryPlan` of `GROUP` over `FETCH`
over `IXSCAN`, and a slot program of a `filter` over an `nlj` whose outer
`ixseek` reads each entry's record id, key, snapshot id and index ident,
and whose inner side is a `limit 1` `seek` of the document. The whole
reply is compared, slot program text and the trial's
`allPlansExecution` included, with each server's collection UUID
replaced by a placeholder; timings are not.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.opdebug import no_timed_yields
from tests.mongod_live.conftest import same
from tests.mongod_live.test_sbe_group_explain import strip

DB = "live_sbe_fetch"

DOCS = [{"a": i, "b": i % 3, "d": -i, "x": [i, i + 1]} for i in range(40)] + [
    {"a": 2.5, "b": 2},
    {"b": 1},
    {"a": None, "b": 2},
]

COUNT = {"$count": "n"}

PIPELINES = [
    [{"$match": {"b": 2, "a": {"$lt": 15}}}, COUNT],
    [{"$match": {"b": 2, "x": {"$gt": 3}}}, COUNT],
    [{"$match": {"b": 7, "a": {"$gt": 1}}}, COUNT],
    [{"$match": {"d": {"$lt": -10}, "x": 12}}, COUNT],
    [{"$match": {"b": 1, "a": {"$gte": 3}}}, {"$group": {"_id": None, "n": {"$sum": 1}}}],
]


def _uuid(db: Any, coll: str) -> str:
    raw = next(c for c in db.list_collections() if c["name"] == coll)["info"]["uuid"]
    return str(raw.as_uuid() if hasattr(raw, "as_uuid") else raw)


def _setup(client: Any) -> Any:
    db = client[DB]
    if "c" not in db.list_collection_names():
        db.c.insert_many([dict(d) for d in DOCS])
        db.c.create_index("a")
        db.c.create_index([("b", -1)])
        db.c.create_index([("d", -1)])
        db.command("create", "v", viewOn="c", pipeline=[{"$match": {"a": {"$gt": 1}}}])
    return db


def _explain(client: Any, cmd: dict, verbosity: str) -> Any:
    db = _setup(client)
    with no_timed_yields(client):
        db.command("explain", cmd, verbosity=verbosity)
        reply = db.command("explain", cmd, verbosity=verbosity)
    return strip(reply, _uuid(db, "c"))


VERBOSITIES = ["queryPlanner", "executionStats", "allPlansExecution"]


@pytest.mark.parametrize("verbosity", VERBOSITIES)
@pytest.mark.parametrize("pipeline", PIPELINES, ids=[str(p) for p in PIPELINES])
def test_fetch_explain_matches_mongod(
    rust_mongo: Any, mongod: Any, pipeline: list, verbosity: str
) -> None:
    cmd = {"aggregate": "c", "pipeline": pipeline, "cursor": {}}
    same(_explain(rust_mongo, cmd, verbosity), _explain(mongod, cmd, verbosity))


COUNTS = [
    {"count": "v", "query": {"b": 2}},
    {"count": "v", "query": {"b": 7}},
    {"count": "v", "query": {"d": {"$gt": -20}}},
    {"aggregate": "v", "pipeline": [{"$match": {"b": 0}}, COUNT], "cursor": {}},
]


@pytest.mark.parametrize("verbosity", VERBOSITIES)
@pytest.mark.parametrize("cmd", COUNTS, ids=[str(c) for c in COUNTS])
def test_count_on_a_view_fetching_explain_matches_mongod(
    rust_mongo: Any, mongod: Any, cmd: dict, verbosity: str
) -> None:
    """`count` on a view whose filter a `FETCH` over an index scan
    finishes: an `nlj` of an `ixseek` and a `limit 1` `seek`."""
    same(_explain(rust_mongo, cmd, verbosity), _explain(mongod, cmd, verbosity))
