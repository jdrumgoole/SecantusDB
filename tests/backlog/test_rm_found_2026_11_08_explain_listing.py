"""Gap tests for how `explain` lists an aggregate's stages, found while
listing the hybrid-search stages (RM-200, 2026-11-08) and compared with a
live mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest


def _explain(client: Any, pipeline: list) -> dict:
    client.drop_database("rmlisting")
    db = client["rmlisting"]
    db.s.insert_many([{"_id": i, "x": i, "y": (i * 7) % 5} for i in range(6)])
    return db.command(
        "explain", {"aggregate": "s", "pipeline": pipeline, "cursor": {}}, verbosity="queryPlanner"
    )


def _union_listing(client: Any) -> list:
    reply = _explain(
        client,
        [{"$unionWith": {"coll": "s", "pipeline": [{"$match": {"x": 1}}, {"$project": {"x": 1}}]}}],
    )
    inner = reply["stages"][1]["$unionWith"]["pipeline"]
    out: list = []
    for stage in inner:
        name = next(iter(stage))
        if name == "$cursor":
            plan = stage["$cursor"]["queryPlanner"]
            out.append([name, plan["parsedQuery"], plan["winningPlan"]["stage"]])
        else:
            out.append(name)
    return out


@pytest.mark.backlog("RM-426")
def test_union_with_sub_pipeline_is_explained(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains a `$unionWith`'s sub-pipeline: a `$cursor` of
    its own (`{x: {$eq: 1}}`, `PROJECTION_SIMPLE` over a `COLLSCAN`), then
    the stages the cursor did not absorb."""
    assert _union_listing(rust_mongo) == _union_listing(mongod)


SORT_THEN_LIMIT: list[list] = [
    [{"$replaceRoot": {"newRoot": {"a": "$x"}}}, {"$sort": {"a": 1}}, {"$limit": 2}],
    [{"$facet": {"f": [{"$sort": {"x": -1}}, {"$limit": 2}]}}],
]


@pytest.mark.backlog("RM-538")
def test_sort_and_limit_after_the_cursor_are_one_stage(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 lists a `$sort` and the `$limit` after it as one
    `$sort` with `limit`, where the cursor does not absorb them: after a
    `$replaceRoot`, and in a `$facet`."""
    for pipeline in SORT_THEN_LIMIT:
        got = _explain(rust_mongo, pipeline)["stages"][1:]
        want = _explain(mongod, pipeline)["stages"][1:]
        assert got == want, pipeline


def _facet_cursor(client: Any) -> str:
    reply = _explain(
        client,
        [
            {
                "$facet": {
                    "f": [
                        {"$setWindowFields": {"sortBy": {"x": -1}, "output": {"r": {"$rank": {}}}}}
                    ]
                }
            }
        ],
    )
    return reply["stages"][0]["$cursor"]["queryPlanner"]["winningPlan"]["stage"]


@pytest.mark.backlog("RM-428")
def test_facet_that_reads_sort_keys_has_a_key_generator(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 plans a `SORT_KEY_GENERATOR` over the scan under a
    `$facet` whose sub-pipeline keeps a sort key (`outputSortKeyMetadata`:
    a `$setWindowFields` `$rank`, a `$rankFusion` input)."""
    assert _facet_cursor(rust_mongo) == _facet_cursor(mongod) == "SORT_KEY_GENERATOR"
