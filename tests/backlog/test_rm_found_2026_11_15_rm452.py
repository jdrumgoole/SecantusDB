"""Gap tests for what working RM-452 (the rewrite of a `$match` after a
`$changeStream` into the oplog filter) left on 2026-11-15, comparing with
mongod 8.2.11 as a one-member replica set.

RM-452 ported mongod's rewrite of `operationType`, RM-698's first slice
those of `documentKey` and `fullDocument`; the rewrites of the other event
fields and of `$expr`, and the Boolean simplifier mongod runs
over the unwind's filter, are still to come.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest


def _explain(client: Any, match: dict, name: str | None = None) -> dict:
    db = client["rm452"]
    name = name or "c" + uuid.uuid4().hex[:8]
    db[name].insert_one({"a": 1})
    pipeline = [{"$changeStream": {}}, {"$match": match}]
    return db.command("aggregate", name, pipeline=pipeline, explain=True)


def _query_hash(client: Any, match: dict) -> str:
    return _explain(client, match)["stages"][0]["$cursor"]["queryPlanner"]["queryHash"]


def _read_and_unwind(client: Any, match: dict) -> tuple:
    db = client["rm452"]
    db["cfixed"].insert_one({"a": 1})
    pipeline = [{"$changeStream": {}}, {"$match": match}]
    stages = db.command("aggregate", "cfixed", pipeline=pipeline, explain=True)["stages"]
    return (
        stages[0]["$cursor"]["queryPlanner"]["queryHash"],
        stages[1]["$changeStream"].get("filter"),
    )


@pytest.mark.backlog_fixed("RM-698")
@pytest.mark.parametrize(
    "match",
    [
        {"fullDocument.a": 1},
        {"fullDocument.a": None},
        {"fullDocument.a": {"$gt": 3}},
        {"fullDocument.a": {"$in": [1, 2]}},
        {"fullDocument.a": {"$ne": 3}},
        {"fullDocument": {"$exists": False}},
        {"documentKey._id": 1},
        {"documentKey._id": None},
        {"documentKey._id": {"$lte": 3}},
        {"documentKey._id": {"$ne": 3}},
        {"documentKey": {"$exists": True}},
        {"$or": [{"documentKey._id": 1}, {"operationType": "drop"}]},
    ],
)
def test_a_match_on_the_document_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict
) -> None:
    """mongod 8.2.11 rewrites a `$match` on `documentKey` (`o2` of an update
    or insert, `o` of a delete) and, inexactly, on `fullDocument` (`o` of an
    insert or replacement, every other update) into the oplog filter and the
    unwind's; this server left them out, reading with hash 90A52575."""
    assert _read_and_unwind(rust_mongo, match) == _read_and_unwind(mongod_rs, match)


@pytest.mark.backlog("RM-698")
@pytest.mark.parametrize(
    ("match", "want"),
    [
        ({"ns.coll": "c"}, "D801EB4B"),
        ({"$expr": {"$eq": ["$operationType", "insert"]}}, "6F8558DD"),
    ],
)
def test_a_match_on_another_event_field_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict, want: str
) -> None:
    """mongod 8.2.11 rewrites a `$match` on `ns` (and `to`,
    `updateDescription`, `fullDocumentBeforeChange`,
    `clusterTime`, `lsid`, `txnNumber`), and an `$expr` on those, into the
    oplog filter, which changes the read's `queryHash` from 90A52575."""
    assert _query_hash(mongod_rs, match) == want
    assert _query_hash(rust_mongo, match) == want


def _unwind(client: Any, match: dict, name: str) -> dict:
    return _explain(client, match, name)["stages"][1]["$changeStream"]["filter"]


@pytest.mark.backlog("RM-699")
@pytest.mark.parametrize("op_type", ["drop", "rename", "dropDatabase"])
def test_the_unwind_filter_is_simplified(rust_mongo: Any, mongod_rs: Any, op_type: str) -> None:
    """mongod 8.2.11 runs its Boolean simplifier over the unwind's filter:
    with `operationType: "drop"` the writes branch (`$nor` of no-ops and
    commands) and the event-type `$or` fall away, since only a command can
    match."""
    match = {"operationType": op_type}
    # One collection name on both sides: the filter spells it out.
    name = "c" + uuid.uuid4().hex[:8]
    want = _unwind(mongod_rs, match, name)
    assert "$nor" not in str(want)
    got = _unwind(rust_mongo, match, name)
    assert got == want
