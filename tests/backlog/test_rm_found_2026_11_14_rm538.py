"""Gap tests for what working RM-538 (the pipeline rewrites `explain`
lists) found on 2026-11-14, comparing with mongod 8.2.11 (macOS arm64)."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

DOCS = [{"_id": i, "x": {"a": i % 3}, "a": i % 3, "b": i} for i in range(6)]


def _explained(client: Any, pipeline: list) -> dict:
    client.drop_database("rm538f")
    db = client.rm538f
    db.c.insert_many(DOCS)
    return db.command(
        "explain", {"aggregate": "c", "pipeline": pipeline, "cursor": {}}, verbosity="queryPlanner"
    )


def _listing(client: Any, pipeline: list) -> dict:
    """The query the read takes and the stages listed after it; a pipeline
    the query takes whole lists none."""
    reply = _explained(client, pipeline)
    if "stages" not in reply:
        return {"parsedQuery": reply["queryPlanner"]["parsedQuery"], "after": []}
    planner = reply["stages"][0]["$cursor"]["queryPlanner"]
    return {"parsedQuery": planner["parsedQuery"], "after": reply["stages"][1:]}


# ------------------------------- a $match the optimizer drops or keeps --

DROPPED_OR_KEPT = {
    "empty_after_group": [{"$group": {"_id": "$a"}}, {"$match": {}}],
    "rate_of_one_after_group": [{"$group": {"_id": "$a"}}, {"$match": {"$sampleRate": 1}}],
    "always_true_after_group": [{"$group": {"_id": "$a"}}, {"$match": {"$alwaysTrue": 1}}],
}


@pytest.mark.backlog("RM-662")
def test_a_match_after_a_group_as_the_optimizer_leaves_it(
    rust_mongo: Any,
    mongod: Any,
) -> None:
    """mongod 8.2.11 drops a `$match` of nothing behind a `$group` -- `{}`,
    or `{$sampleRate: 1}`, which it parses into nothing -- and the query
    then takes the whole pipeline; it leaves `{$alwaysTrue: 1}` behind the
    `$group`. This server lists the empty `$match`, and moves the
    `$alwaysTrue` into the query."""

    def stage_names(client: Any) -> dict:
        out = {}
        for name, pipeline in DROPPED_OR_KEPT.items():
            reply = _explained(client, pipeline)
            out[name] = [next(iter(s)) for s in reply.get("stages", [])] or "query takes all"
        return out

    assert stage_names(rust_mongo) == stage_names(mongod)


# ------------------------------------------------ a $$ROOT.x new root --


@pytest.mark.backlog("RM-662")
def test_a_match_moves_ahead_of_a_root_variable_new_root(
    rust_mongo: Any,
    mongod: Any,
) -> None:
    """mongod 8.2.11 moves a `$match` behind `{$replaceRoot: {newRoot:
    "$$ROOT.x"}}` into the query, renamed onto `x`, as it does behind
    `"$x"` (RM-536). This server leaves it behind the `$replaceRoot`."""
    pipeline = [{"$replaceRoot": {"newRoot": "$$ROOT.x"}}, {"$match": {"a": 1}}]
    assert _listing(rust_mongo, pipeline) == _listing(mongod, pipeline)


# ------------------------------------------- an invalid $sampleRate --


@pytest.mark.backlog("RM-663")
def test_explain_refuses_a_sample_rate_the_run_refuses(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses to explain `{$sampleRate: 2}` (BadValue,
    "numeric argument to $sampleRate must be in [0, 1]") and `{$sampleRate:
    "x"}` (BadValue, "argument to $sampleRate must be a numeric type"), as
    it refuses to run them. This server runs neither, but explains both."""

    def answers(client: Any) -> list:
        return [
            mongo_answer(lambda r=rate: _listing(client, [{"$match": {"$sampleRate": r}}]))
            for rate in (2, "x")
        ]

    assert answers(rust_mongo) == answers(mongod)


# ------------------------------------------- what a top-k sort sizes --


@pytest.mark.backlog("RM-689")
def test_a_top_k_sort_reports_the_documents_it_keeps(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$sort` that took a `$limit` reports the size of the
    documents it keeps -- 490 for two of three `$documents` documents, 0
    for a `limit` of 1, in a `$facet` too. This server reports the size of
    all three it sorted (735)."""
    docs = {"$documents": [{"a": 1}, {"a": 2}, {"a": 3}]}
    pipelines = {
        "limit_2": [docs, {"$sort": {"a": -1}}, {"$limit": 2}],
        "limit_1": [docs, {"$sort": {"a": -1}}, {"$limit": 1}],
        "in_a_facet": [docs, {"$facet": {"f": [{"$sort": {"a": -1}}, {"$limit": 2}]}}],
    }

    def sizes(client: Any) -> dict:
        out = {}
        for name, pipeline in pipelines.items():
            reply = client.admin.command(
                "explain",
                {"aggregate": 1, "pipeline": pipeline, "cursor": {}},
                verbosity="executionStats",
            )
            stages = reply["stages"]
            if name == "in_a_facet":
                stages = stages[-1]["$facet"]["f"]
            out[name] = [s.get("totalDataSizeSortedBytesEstimate") for s in stages if "$sort" in s]
        return out

    assert sizes(rust_mongo) == sizes(mongod)
