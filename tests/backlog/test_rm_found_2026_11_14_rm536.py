"""Gap tests for what working RM-536 (a `$match` moved ahead of a
`$replaceRoot` over a collection) found on 2026-11-14, comparing with mongod
8.2.11 (macOS arm64).
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

# The `$or` RM-536's rewrite builds, beside a clause an index takes.
_GUARDED = [{"x.a": 1}, {"x": {"$type": [4]}}, {"x": {"$not": {"$type": [3]}}}]


def _fetch_filters(client: Any) -> dict:
    coll = client["rm_found_1114"]["c" + uuid.uuid4().hex[:8]]
    coll.insert_many([{"_id": i, "k": i, "x": {"a": i % 2}} for i in range(1, 6)])
    coll.create_index("k")

    def fetch_filter(command: dict) -> Any:
        reply = coll.database.command("explain", command, verbosity="queryPlanner")
        planner = reply.get("queryPlanner") or reply["stages"][0]["$cursor"]["queryPlanner"]
        stage = planner["winningPlan"]
        while stage.get("stage") != "FETCH":
            stage = stage["inputStage"]
        return stage.get("filter")

    return {
        "find": fetch_filter({"find": coll.name, "filter": {"k": {"$gte": 2}, "$or": _GUARDED}}),
        "aggregate": fetch_filter(
            {
                "aggregate": coll.name,
                "pipeline": [
                    {"$match": {"k": {"$gte": 2}}},
                    {"$replaceRoot": {"newRoot": "$x"}},
                    {"$match": {"a": 1}},
                ],
                "cursor": {},
            }
        ),
    }


@pytest.mark.backlog("RM-658")
def test_a_residual_or_lists_its_branches_as_mongod_does(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 lists the `$or` a FETCH filters by, behind an IXSCAN of
    `k`, as `[{x: {$not: {$type: [3]}}}, {x: {$type: [4]}}, {"x.a": {$eq:
    1}}]` for `{k: {$gte: 2}, $or: [{"x.a": 1}, {x: {$type: [4]}}, {x:
    {$not: {$type: [3]}}}]}`, in a `find` and in the `$or` an aggregate's
    `$replaceRoot` rewrite builds alike, though its `parsedQuery` sorts the
    branches `x.a` first. This server lists the FETCH's `$or` as
    `parsedQuery` does."""
    want = _fetch_filters(mongod)
    assert want["find"] == {
        "$or": [{"x": {"$not": {"$type": [3]}}}, {"x": {"$type": [4]}}, {"x.a": {"$eq": 1}}]
    }
    assert _fetch_filters(rust_mongo) == want


def _replace_root_error(client: Any) -> tuple:
    coll = client["rm_found_1114"]["c" + uuid.uuid4().hex[:8]]
    coll.insert_many([{"_id": 1, "x": {"a": 1, "b": 1}}])
    return mongo_answer(
        lambda: coll.database.command(
            "explain",
            {
                "aggregate": coll.name,
                "pipeline": [{"$replaceRoot": {"newRoot": "$x.b"}}],
                "cursor": {},
            },
            verbosity="executionStats",
        )["ok"]
    )


@pytest.mark.backlog("RM-659")
def test_explain_reports_a_replace_roots_run_time_error(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs the pipeline for `executionStats` and returns the
    executor's error: `{$replaceRoot: {newRoot: "$x.b"}}` over `{x: {a: 1,
    b: 1}}` is 40228, naming the document the read handed it (`{x: {b:
    1}}`), with no "Executor error" prefix. This server answers with an
    `explain` document: `count_inline` takes a failed run for 0 documents."""
    want = _replace_root_error(mongod)
    assert want[:2] == ("err", 40228)
    assert _replace_root_error(rust_mongo) == want
