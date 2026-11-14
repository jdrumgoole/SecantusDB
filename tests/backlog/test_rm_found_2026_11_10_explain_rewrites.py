"""Gap tests for what working RM-414 (a collectionless aggregate's
explain) found on 2026-11-10, comparing with mongod 8.2.11 (a one-member
replica set, macOS arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

DOCS = {"$documents": [{"a": 1, "b": 3}, {"a": 2}]}


def _without_times(value: Any) -> Any:
    """The answer less each stage's time, which is the machine's."""
    if isinstance(value, dict):
        return {
            k: _without_times(v) for k, v in value.items() if k != "executionTimeMillisEstimate"
        }
    if isinstance(value, list):
        return [_without_times(v) for v in value]
    return value


def _stages(client: Any, pipelines: dict, verbosity: str) -> dict:
    def explained(pipeline: list) -> Any:
        reply = client.admin.command(
            "explain",
            {"aggregate": 1, "pipeline": pipeline, "cursor": {}},
            verbosity=verbosity,
        )
        return _without_times(reply["stages"])

    return {name: mongo_answer(lambda p=p: explained(p)) for name, p in pipelines.items()}


# ------------------------------ a $replaceRoot over a collection --


def _replace_root_over_a_collection(client: Any) -> Any:
    client.drop_database("rm414c")
    db = client.rm414c
    db.c.insert_many([{"x": {"a": 1}}, {"x": {"a": 2}}])

    def explained() -> dict:
        reply = db.command(
            "explain",
            {
                "aggregate": "c",
                "pipeline": [{"$replaceRoot": {"newRoot": "$x"}}, {"$match": {"a": 1}}],
                "cursor": {},
            },
            verbosity="queryPlanner",
        )
        planner = reply["stages"][0]["$cursor"]["queryPlanner"]
        return {"parsedQuery": planner["parsedQuery"], "after": reply["stages"][1:]}

    return mongo_answer(explained)


@pytest.mark.backlog_fixed("RM-536")
def test_a_match_moves_ahead_of_a_replace_root_into_the_query(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 moves a `$match` behind `{$replaceRoot: {newRoot:
    "$x"}}` ahead of it, renamed onto `x` in an `$or` with `{x: {$type:
    [4]}}` and `{x: {$not: {$type: [3]}}}`, and the collection's query takes
    it (`parsedQuery` is that `$or`). This server left the `$match` behind
    the `$replaceRoot` and read the whole collection until 2026-11-14: the
    rewrite ran only after a pipeline's own source (RM-414)."""
    assert _replace_root_over_a_collection(rust_mongo) == _replace_root_over_a_collection(mongod_rs)


# ----------------------------------- a $match listed as written --

AS_WRITTEN = {
    "behind_add_fields": [
        DOCS,
        {"$addFields": {"c": 1, "d": 1}},
        {"$match": {"d": {"$gt": 0}, "c": 1}},
    ],
    "an_expr": [DOCS, {"$match": {"$expr": {"$eq": ["$a", 1]}}}],
    "moved_ahead": [DOCS, {"$match": {"b": {"$gt": 0}, "a": 1}}],
    "a_range": [DOCS, {"$match": {"a": {"$gt": 0, "$lt": 5}}}],
}


@pytest.mark.backlog_fixed("RM-537")
def test_a_pipeline_match_lists_its_clauses_as_written(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 lists a pipeline's `$match` with its clauses in the
    order they were written (`{d: {$gt: 0}, c: 1}` as `$and: [d, c]`, an
    `$expr`'s `$_internalExprEq` ahead of the `$expr`); only a query's
    `parsedQuery` is sorted. This server sorted them as `parsedQuery` does
    until 2026-11-14."""
    assert _stages(rust_mongo, AS_WRITTEN, "queryPlanner") == _stages(
        mongod_rs, AS_WRITTEN, "queryPlanner"
    )


# ------------------------------------- the optimizer's rewrites --

REWRITES = {
    "replace_with_root": [{"$documents": [{"a": 1}]}, {"$replaceWith": "$$ROOT"}],
    "sort_then_limit": [DOCS, {"$sort": {"a": -1}}, {"$limit": 1}],
    "through_a_rename": [{"$documents": [{"a": 1}]}, {"$set": {"b": "$a"}}, {"$match": {"b": 1}}],
    "sample_rate": [DOCS, {"$addFields": {"c": 1}}, {"$match": {"$sampleRate": 0.5}}],
}


@pytest.mark.backlog_fixed("RM-538")
def test_the_pipeline_rewrites_mongod_lists(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 drops a `$replaceWith: "$$ROOT"`, folds a `$limit`
    into the `$sort` before it (`{$sort: {sortKey, limit: 1}}`), moves a
    `$match` on a field an `$addFields` / `$set` renames ahead of it onto
    the field it names, and lists `$sampleRate` as the `$expr` of `$rand`
    it is. This server listed each stage as written until 2026-11-14."""
    assert _stages(rust_mongo, REWRITES, "queryPlanner") == _stages(
        mongod_rs, REWRITES, "queryPlanner"
    )


# ----------------------------------------- what a stage reports --

DETAIL = {
    "facet": [DOCS, {"$facet": {"x": [{"$match": {"a": 1}}]}}],
    "union_with": [
        {"$documents": [{"a": 1}]},
        {"$unionWith": {"pipeline": [{"$documents": [{"a": 2}]}]}},
    ],
    "lookup": [
        {"$documents": [{"a": 1}]},
        {"$lookup": {"pipeline": [{"$documents": [{"b": 1}]}], "as": "z"}},
    ],
    "count": [DOCS, {"$count": "n"}],
    "sort": [DOCS, {"$sort": {"a": 1}}],
}


@pytest.mark.backlog_fixed("RM-539")
def test_each_stage_reports_what_mongod_does(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11's `executionStats` give the stages inside a `$facet`
    and a `$unionWith` their own counts (and list the `$unionWith`'s
    `$documents` as the stages it runs), a `$lookup` its
    `totalDocsExamined`, `totalKeysExamined`, `collectionScans` and
    `indexesUsed`, the `$group` a `$count` makes its memory figures, and a
    `$sort` after `$documents` mongod's own size estimate. This server left
    those out, and estimated the sort's size from the documents' BSON,
    until 2026-11-14."""
    assert _stages(rust_mongo, DETAIL, "executionStats") == _stages(
        mongod_rs, DETAIL, "executionStats"
    )
