"""`explain` of a `$changeStream` against a one-member mongod 8.2 replica
set (RM-076): the read of `local.oplog.rs` -- its filter, plan and hashes --
and the internal stages the stream expands into, for each scope and each
option that changes them.

What is per-server by nature is taken out and checked on each side alone:
the time the stream starts from (the cluster time, or the resume token's),
which the filter's `ts`, the scan's `minRecord` and `internalTransform`'s
`startAtOperationTime` must all agree on; the resume token (this server's
tokens are its own format, `tasks/backlog-rust-mongodb.md` 3.3); and the
timings. mongod's `queryShapeHash` is dropped (RM-454), and no case uses a
`$match` mongod rewrites into the oplog filter (RM-452) or a stream started
before the end of the oplog under `executionStats` (RM-453).
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import Timestamp

from tests.mongod_live.conftest import same

DB = "live_cs_explain"

_TIMINGS = ("executionTimeMillis", "executionTimeMillisEstimate", "optimizationTimeMillis")


def _token(client: Any) -> dict:
    """The resume token of an insert into `DB.c`, made on this server."""
    coll = client[DB]["c"]
    with coll.watch() as stream:
        coll.insert_one({"a": 2})
        return stream.next()["_id"]


def _start_of(stage: dict) -> Timestamp:
    """The start time `internalTransform` was given, or its resume token's
    (checked against the filter by the caller)."""
    return stage["$changeStream"]["options"].get("startAtOperationTime")


def _normalized(reply: dict) -> dict:
    for k in ("serverInfo", "serverParameters", "command", "$clusterTime", "operationTime"):
        reply.pop(k, None)
    # RM-454: this server prints no `queryShapeHash` for a change stream.
    reply.pop("queryShapeHash", None)
    stages = reply["stages"]
    planner = stages[0]["$cursor"]["queryPlanner"]
    clauses = planner["parsedQuery"]["$and"]
    (ts,) = [c["ts"]["$gte"] for c in clauses if "ts" in c]
    scan = planner["winningPlan"]["inputStage"]
    assert scan["minRecord"] == (ts.time << 32) | ts.inc, (scan["minRecord"], ts)
    assert scan["filter"] == planner["parsedQuery"]

    def scrub(node: Any) -> Any:
        if isinstance(node, dict):
            out = {}
            for k, v in node.items():
                if k in ("resumeToken", "resumeAfter", "startAfter"):
                    out[k] = "TOKEN"
                elif k in _TIMINGS:
                    out[k] = 0
                elif k == "minRecord" or (isinstance(v, Timestamp) and v == ts):
                    out[k] = "START"
                else:
                    out[k] = scrub(v)
            return out
        if isinstance(node, list):
            return [scrub(v) for v in node]
        return node

    for stage in stages:
        options = stage.get("$changeStream", {}).get("options")
        if options is not None and "startAtOperationTime" in options:
            assert _start_of(stage) == ts
    return scrub(reply)


def _explain(client: Any, case: dict, verbosity: str | None) -> dict:
    client[DB]["c"].insert_one({"a": 1})
    spec = dict(case["spec"])
    for k in ("resumeAfter", "startAfter"):
        if spec.get(k) == "TOKEN":
            spec[k] = _token(client)
    db, target = case.get("on", (DB, "c"))
    pipeline = [{"$changeStream": spec}, *case.get("then", [])]
    if verbosity is None:
        return _normalized(client[db].command("aggregate", target, pipeline=pipeline, explain=True))
    command = {"aggregate": target, "pipeline": pipeline, "cursor": {}}
    return _normalized(client[db].command("explain", command, verbosity=verbosity))


CASES = {
    "collection": {"spec": {}},
    "database": {"spec": {}, "on": (DB, 1)},
    "cluster": {"spec": {"allChangesForCluster": True}, "on": ("admin", 1)},
    "expanded": {"spec": {"showExpandedEvents": True}},
    "system": {"spec": {"showSystemEvents": True}},
    "migration": {"spec": {"showMigrationEvents": True}},
    "database expanded system": {
        "spec": {"showExpandedEvents": True, "showSystemEvents": True},
        "on": (DB, 1),
    },
    "cluster expanded": {
        "spec": {"allChangesForCluster": True, "showExpandedEvents": True},
        "on": ("admin", 1),
    },
    "cluster system": {
        "spec": {"allChangesForCluster": True, "showSystemEvents": True},
        "on": ("admin", 1),
    },
    "update lookup": {"spec": {"fullDocument": "updateLookup"}},
    "post-images": {"spec": {"fullDocument": "whenAvailable"}},
    "pre- and post-images": {
        "spec": {"fullDocumentBeforeChange": "required", "fullDocument": "required"}
    },
    "every flag": {
        "spec": {
            "showRawUpdateDescription": False,
            "showExpandedEvents": False,
            "showSystemEvents": False,
            "allChangesForCluster": False,
        }
    },
    "a start time": {"spec": {"startAtOperationTime": Timestamp(1700000000, 5)}},
    "resumed": {"spec": {"resumeAfter": "TOKEN"}},
    "started after": {"spec": {"startAfter": "TOKEN", "fullDocument": "updateLookup"}},
    "a $match": {"spec": {}, "then": [{"$match": {"x": 1}}, {"$match": {"y": 2}}]},
    "a $match behind a $project": {
        "spec": {},
        "then": [{"$project": {"x": 1}}, {"$match": {"x": 1}}],
    },
    "a $match on the post-image": {
        "spec": {"fullDocument": "updateLookup"},
        "then": [{"$match": {"x": 1}}],
    },
    "a $match behind a $set, expanded": {
        "spec": {"showExpandedEvents": True, "fullDocumentBeforeChange": "whenAvailable"},
        "then": [{"$set": {"x": 1}}, {"$match": {"x": 1, "y": 2}}],
    },
    "reshaping stages": {
        "spec": {},
        "then": [{"$unset": "y"}, {"$addFields": {"z": "$x"}}, {"$redact": "$$KEEP"}],
    },
    "resumed and split": {
        "spec": {"resumeAfter": "TOKEN", "fullDocumentBeforeChange": "whenAvailable"},
        "then": [{"$project": {"x": 1}}, {"$changeStreamSplitLargeEvent": {}}],
    },
}


@pytest.mark.parametrize("name", list(CASES))
def test_explain_of_a_change_stream(rust_mongo: Any, mongod_rs: Any, name: str) -> None:
    same(_explain(rust_mongo, CASES[name], None), _explain(mongod_rs, CASES[name], None))


@pytest.mark.parametrize("verbosity", ["queryPlanner", "executionStats", "allPlansExecution"])
@pytest.mark.parametrize("name", ["collection", "database", "pre- and post-images"])
def test_explain_of_a_change_stream_at_each_verbosity(
    rust_mongo: Any, mongod_rs: Any, name: str, verbosity: str
) -> None:
    """A new stream, which starts after the last entry and reads nothing."""
    same(
        _explain(rust_mongo, CASES[name], verbosity),
        _explain(mongod_rs, CASES[name], verbosity),
    )
