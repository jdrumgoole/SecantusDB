"""How `explain` of an aggregate over a collection moves a `$match`, a
`$skip` and a `$limit` ahead of a `$replaceRoot` / `$replaceWith` of a
field path (RM-536).

mongod 8.2 rewrites a `$match` behind `{$replaceRoot: {newRoot: "$x"}}`
onto `x` (`ReplaceRootTransformation`), in an `$or` with `{x: {$type:
[4]}}` and `{x: {$not: {$type: [3]}}}` so that what the `$replaceRoot`
refuses still reaches it, and the collection's query takes the `$or` (a
rooted `$or`: `SUBPLAN`). A `$skip` or `$limit` goes ahead of it too, into
the query. A `$match` a `$limit` keeps out of the query is listed as the
rewrite left it, not sorted. (Joined with a `$match` an index takes, the
`$or` is the FETCH's residual, which this server lists in another order:
a backlog gap test, not here.) Inside a change stream the moved `$match`
joins the event-type filter. Compared are the parsed query, the winning
plan's stages, the stages listed after the read and the documents.
Measured on 8.2.11, 2026-11-14.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.mongod_live.conftest import same

PIPELINES: dict[str, list[dict]] = {
    "replace_root_match": [{"$replaceRoot": {"newRoot": "$x"}}, {"$match": {"a": 1}}],
    "replace_with_match": [{"$replaceWith": "$x"}, {"$match": {"a": 1}}],
    "two_clauses": [{"$replaceRoot": {"newRoot": "$x"}}, {"$match": {"b": {"$gt": 0}, "a": 1}}],
    "an_or": [{"$replaceRoot": {"newRoot": "$x"}}, {"$match": {"$or": [{"a": 1}, {"b": 2}]}}],
    "an_in": [{"$replaceRoot": {"newRoot": "$x"}}, {"$match": {"a": {"$in": [1, 2]}}}],
    "through_a_sort": [
        {"$replaceRoot": {"newRoot": "$x"}},
        {"$sort": {"a": 1}},
        {"$match": {"a": 1}},
    ],
    "then_a_sort": [
        {"$replaceRoot": {"newRoot": "$x"}},
        {"$match": {"a": 1}},
        {"$sort": {"b": -1}},
    ],
    "after_a_projection": [
        {"$project": {"x": 1}},
        {"$replaceRoot": {"newRoot": "$x"}},
        {"$match": {"a": 1}},
    ],
    "skip": [{"$replaceRoot": {"newRoot": "$x"}}, {"$skip": 1}],
    "limit": [{"$replaceRoot": {"newRoot": "$x"}}, {"$limit": 2}],
    "skip_limit": [{"$replaceRoot": {"newRoot": "$x"}}, {"$skip": 1}, {"$limit": 2}],
    "behind_a_limit": [
        {"$limit": 3},
        {"$replaceRoot": {"newRoot": "$x"}},
        {"$match": {"a": 1}},
    ],
}


def _strip(plan: dict) -> dict:
    """The plan's stages, with what each does; the rest is per-run."""
    keep = ("stage", "transformBy", "limitAmount", "skipAmount", "filter")
    out = {k: v for k, v in plan.items() if k in keep}
    if "inputStage" in plan:
        out["inputStage"] = _strip(plan["inputStage"])
    return out


def _explained(client: Any) -> dict:
    coll = client["live_replace_root"]["c" + uuid.uuid4().hex[:8]]
    coll.insert_many([{"_id": i, "x": {"a": i % 2, "b": i}} for i in range(1, 6)])
    out = {}
    for name, pipeline in PIPELINES.items():
        reply = coll.database.command(
            "explain",
            {"aggregate": coll.name, "pipeline": pipeline, "cursor": {}},
            verbosity="queryPlanner",
        )
        planner = reply["stages"][0]["$cursor"]["queryPlanner"]
        out[name] = {
            "parsedQuery": planner["parsedQuery"],
            "plan": _strip(planner["winningPlan"]),
            "after": reply["stages"][1:],
            "docs": list(coll.aggregate(pipeline)),
        }
    return out


def test_a_replace_root_takes_the_match_and_paging_behind_it(rust_mongo: Any, mongod: Any) -> None:
    ours, theirs = _explained(rust_mongo), _explained(mongod)
    for name in PIPELINES:
        same(ours[name], theirs[name])


def _change_stream_tail(client: Any, pipeline: list[dict]) -> list[dict]:
    db = client["live_replace_root_cs"]
    db.c.replace_one({"_id": 0}, {"_id": 0}, upsert=True)
    reply = db.command(
        "explain",
        {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
        verbosity="queryPlanner",
    )
    # The stages from the event-type `$match` on: the resume token and the
    # start time before it are each server's own.
    stages = reply["stages"]
    first = next(i for i, s in enumerate(stages) if "$match" in s)
    return stages[first:]


@pytest.mark.parametrize(
    "pipeline",
    [
        [
            {"$changeStream": {}},
            {"$replaceRoot": {"newRoot": "$fullDocument"}},
            {"$match": {"a": 1}},
        ],
        [
            {"$changeStream": {"fullDocument": "updateLookup"}},
            {"$replaceWith": "$fullDocument"},
            {"$match": {"a": 1}},
        ],
    ],
    ids=["joins_the_event_types", "behind_the_post_image"],
)
def test_a_change_stream_moves_the_match_ahead_of_a_replace_root(
    rust_mongo: Any,
    mongod_rs: Any,
    pipeline: list[dict],
) -> None:
    same(_change_stream_tail(rust_mongo, pipeline), _change_stream_tail(mongod_rs, pipeline))
