"""`explain` of a `$count` (or a `$group` that reads no field) over an index
scan that needs no fetch, and of `count` on a view so served (RM-072).

mongod 8.2.11 turns the plan's `FETCH` / `IXSCAN` into a `COUNT_SCAN` when
the group reads no field, and runs it in SBE as an `ixseek` between two
KeyStrings: `explainVersion` "2", a `queryPlan` of `GROUP` over
`COUNT_SCAN`, and executionStats whose `ixseek` counts the keys it read.
The whole reply is compared, slot program text included, with each
server's collection UUID replaced by a placeholder; timings are not.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.opdebug import no_timed_yields
from tests.mongod_live.conftest import same
from tests.mongod_live.test_sbe_group_explain import strip

DB = "live_sbe_count_scan"

DOCS = [{"a": i, "b": i % 3, "d": -i} for i in range(40)] + [
    {"a": 2.5},
    {"a": -7},
    {"x": 1},
    {"a": None},
    {"a": "str"},
    {"a": True},
]

COUNT = {"$count": "n"}

PIPELINES = [
    [{"$match": {"a": {"$gt": 1}}}, COUNT],
    [{"$match": {"a": {"$gte": 2}}}, COUNT],
    [{"$match": {"a": {"$lt": 5}}}, COUNT],
    [{"$match": {"a": {"$lte": 2.5}}}, COUNT],
    [{"$match": {"a": {"$gt": 1, "$lt": 30}}}, COUNT],
    [{"$match": {"a": 7}}, COUNT],
    [{"$match": {"a": "str"}}, COUNT],
    [{"$match": {"a": None}}, COUNT],
    [{"$match": {"a": {"$gt": 100}}}, COUNT],
    [{"$match": {"d": {"$lt": -10}}}, COUNT],
    [{"$match": {"d": {"$gte": -3}}}, COUNT],
    [{"$match": {"a": {"$gt": 1}}}, {"$group": {"_id": None, "n": {"$sum": 1}}}],
    [{"$match": {"a": {"$gt": 1}}}, {"$group": {"_id": None, "n": {"$count": {}}}}],
]


def _uuid(db: Any, coll: str) -> str:
    raw = next(c for c in db.list_collections() if c["name"] == coll)["info"]["uuid"]
    return str(raw.as_uuid() if hasattr(raw, "as_uuid") else raw)


def _setup(client: Any) -> Any:
    db = client[DB]
    if "c" not in db.list_collection_names():
        db.c.insert_many([dict(d) for d in DOCS])
        db.c.create_index("a")
        db.c.create_index([("d", -1)])
        db.command("create", "v", viewOn="c", pipeline=[{"$match": {"a": {"$gt": 1}}}])
        db.command("create", "vd", viewOn="c", pipeline=[{"$match": {"d": {"$lte": -5}}}])
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
def test_count_scan_explain_matches_mongod(
    rust_mongo: Any, mongod: Any, pipeline: list, verbosity: str
) -> None:
    cmd = {"aggregate": "c", "pipeline": pipeline, "cursor": {}}
    same(_explain(rust_mongo, cmd, verbosity), _explain(mongod, cmd, verbosity))


COUNTS = [
    {"count": "v"},
    {"count": "vd"},
    {"count": "v", "query": {"a": {"$lt": 30}}},
    {"aggregate": "v", "pipeline": [COUNT], "cursor": {}},
]


@pytest.mark.parametrize("verbosity", VERBOSITIES)
@pytest.mark.parametrize("cmd", COUNTS, ids=[str(c) for c in COUNTS])
def test_count_on_a_view_over_an_index_explain_matches_mongod(
    rust_mongo: Any, mongod: Any, cmd: dict, verbosity: str
) -> None:
    """`count` on a view whose filter an index serves whole: a `GROUP` over
    a `COUNT_SCAN`, run as an `ixseek`."""
    same(_explain(rust_mongo, cmd, verbosity), _explain(mongod, cmd, verbosity))
