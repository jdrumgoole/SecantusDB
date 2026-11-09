"""Gap tests for what working RM-076 (`explain` of a `$changeStream`) left
on 2026-11-09, comparing with mongod 8.2.11 as a one-member replica set.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest


def _planner(client: Any, then: list) -> dict:
    db = client["rm449"]
    name = "c" + uuid.uuid4().hex[:8]
    db[name].insert_one({"a": 1})
    pipeline = [{"$changeStream": {}}, *then]
    e = db.command("aggregate", name, pipeline=pipeline, explain=True)
    return e["stages"][0]["$cursor"]["queryPlanner"]


@pytest.mark.backlog("RM-452")
def test_a_match_on_an_event_field_joins_the_oplog_filter(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 rewrites a `$match` on `operationType` into the oplog
    filter (`operationType: "insert"` adds `op: {$eq: "i"}` beside the
    writes), which changes the read's shape: its `queryHash` is 2CFDBDA8
    rather than 90A52575."""
    then = [{"$match": {"operationType": "insert"}}]
    want = _planner(mongod_rs, then)["queryHash"]
    assert want == "2CFDBDA8"
    assert _planner(rust_mongo, then)["queryHash"] == want


def _counted(client: Any) -> tuple:
    db = client["rm450_" + uuid.uuid4().hex[:6]]
    db.create_collection("c")
    start = client.admin.command("hello")["lastWrite"]["opTime"]["ts"]
    for i in range(3):
        db.c.insert_one({"i": i})
    command = {
        "aggregate": "c",
        "pipeline": [{"$changeStream": {"startAtOperationTime": start}}],
        "cursor": {},
    }
    e = db.command("explain", command, verbosity="executionStats")
    stats = e["stages"][0]["$cursor"]["executionStats"]
    return stats["nReturned"], [s["nReturned"] for s in e["stages"]]


@pytest.mark.backlog("RM-453")
def test_execution_stats_count_what_the_read_found(rust_mongo: Any, mongod_rs: Any) -> None:
    """A stream that starts before three inserts: mongod 8.2.11's
    `executionStats` reads the oplog from the start time and counts the
    three entries at every stage."""
    want = _counted(mongod_rs)
    assert want == (3, [3, 3, 3, 3, 3, 3])
    assert _counted(rust_mongo) == want


def _shape_hash(client: Any) -> Any:
    db = client["rm451"]
    db.c.insert_one({"a": 1})
    return db.command("aggregate", "c", pipeline=[{"$changeStream": {}}], explain=True).get(
        "queryShapeHash"
    )


@pytest.mark.backlog_fixed("RM-454")
def test_a_change_stream_has_a_query_shape_hash(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 prints a `queryShapeHash` for the explain of a
    collection or database change stream (none for a cluster one). This
    server printed none until `query_shape.rs` shaped `$changeStream`
    (RM-358)."""
    want = _shape_hash(mongod_rs)
    assert want is not None
    assert _shape_hash(rust_mongo) == want
