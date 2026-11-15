"""Gap tests for what working RM-452 (the rewrite of a `$match` after a
`$changeStream` into the oplog filter) left on 2026-11-15, comparing with
mongod 8.2.11 as a one-member replica set.

RM-452 ported mongod's rewrite of `operationType`; the rewrites of the
other event fields and of `$expr`, and the Boolean simplifier mongod runs
over the unwind's filter, are still to come.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest


def _explain(client: Any, match: dict) -> dict:
    db = client["rm452"]
    name = "c" + uuid.uuid4().hex[:8]
    db[name].insert_one({"a": 1})
    pipeline = [{"$changeStream": {}}, {"$match": match}]
    return db.command("aggregate", name, pipeline=pipeline, explain=True)


def _query_hash(client: Any, match: dict) -> str:
    return _explain(client, match)["stages"][0]["$cursor"]["queryPlanner"]["queryHash"]


@pytest.mark.backlog("RM-698")
@pytest.mark.parametrize(
    ("match", "want"),
    [
        ({"fullDocument.a": 1}, "193DCAB5"),
        ({"documentKey._id": 1}, "97E310BC"),
        ({"ns.coll": "c"}, "D801EB4B"),
        ({"$expr": {"$eq": ["$operationType", "insert"]}}, "6F8558DD"),
    ],
)
def test_a_match_on_another_event_field_joins_the_oplog_filter(
    rust_mongo: Any, mongod_rs: Any, match: dict, want: str
) -> None:
    """mongod 8.2.11 rewrites a `$match` on `fullDocument`, `documentKey`,
    `ns` (and `to`, `updateDescription`, `fullDocumentBeforeChange`,
    `clusterTime`, `lsid`, `txnNumber`), and an `$expr` on those, into the
    oplog filter, which changes the read's `queryHash` from 90A52575."""
    assert _query_hash(mongod_rs, match) == want
    assert _query_hash(rust_mongo, match) == want


def _unwind(client: Any, match: dict) -> dict:
    return _explain(client, match)["stages"][1]["$changeStream"]["filter"]


@pytest.mark.backlog("RM-699")
@pytest.mark.parametrize("op_type", ["drop", "rename", "dropDatabase"])
def test_the_unwind_filter_is_simplified(rust_mongo: Any, mongod_rs: Any, op_type: str) -> None:
    """mongod 8.2.11 runs its Boolean simplifier over the unwind's filter:
    with `operationType: "drop"` the writes branch (`$nor` of no-ops and
    commands) and the event-type `$or` fall away, since only a command can
    match."""
    match = {"operationType": op_type}
    want = _unwind(mongod_rs, match)
    assert "$nor" not in str(want)
    got = _unwind(rust_mongo, match)
    assert got == want
