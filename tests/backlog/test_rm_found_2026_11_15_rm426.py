"""Gap tests found while explaining a `$unionWith`'s sub-pipeline (RM-426,
2026-11-15), compared with a live mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

HASHES = ("queryHash", "planCacheShapeHash", "planCacheKey")


def _hashes(client: Any) -> tuple:
    client.drop_database("rm426h")
    db = client.rm426h
    db.d.insert_many([{"_id": n, "x": n} for n in range(1, 7)])
    db.d.create_index("x")
    reply = db.command(
        "explain",
        {
            "aggregate": "d",
            "pipeline": [{"$match": {"x": {"$gt": 2}}}, {"$group": {"_id": "$x"}}],
            "cursor": {},
        },
        verbosity="queryPlanner",
    )
    planner = reply["stages"][0]["$cursor"]["queryPlanner"]
    return tuple(planner[k] for k in HASHES)


@pytest.mark.backlog("RM-697")
def test_a_distinct_scan_group_hashes_as_mongod_does(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 hashes a `$group` it reads by a `DISTINCT_SCAN`
    (`76A99463` / `09B22F21`); this server hashes the same plan as another
    query."""
    assert _hashes(rust_mongo) == _hashes(mongod)


RANK_FUSION = {
    "$rankFusion": {
        "input": {
            "pipelines": {
                "a": [{"$sort": {"x": 1}}],
                "b": [{"$match": {"x": {"$gt": 2}}}, {"$sort": {"x": -1}}, {"$limit": 4}],
            }
        }
    }
}

SCORE_FUSION = {
    "$scoreFusion": {
        "input": {
            "pipelines": {
                "a": [{"$score": {"score": "$x"}}],
                "b": [
                    {"$match": {"x": {"$lt": 5}}},
                    {"$score": {"score": {"$multiply": ["$x", 2]}}},
                ],
            },
            "normalization": "none",
        }
    }
}


def _fusion_explain(client: Any, stage: dict, verbosity: str = "queryPlanner") -> dict:
    client.drop_database("rm426f")
    db = client.rm426f
    db.d.insert_many([{"_id": n, "x": n} for n in range(1, 7)])
    return db.command(
        "explain",
        {"aggregate": "d", "pipeline": [stage], "cursor": {}},
        verbosity=verbosity,
    )


def _union_cursors(reply: dict) -> list:
    """Each `$unionWith`'s `$cursor` query planner, less the timings, and
    the names of the stages after it."""
    out = []
    for stage in reply["stages"]:
        if "$unionWith" not in stage:
            continue
        pipeline = stage["$unionWith"]["pipeline"]
        head = pipeline[0].get("$cursor", {}).get("queryPlanner", {})
        head = {k: v for k, v in head.items() if k != "optimizationTimeMillis"}
        out.append((head, [next(iter(s)) for s in pipeline[1:]]))
    return out


@pytest.mark.backlog_fixed("RM-696")
@pytest.mark.parametrize("stage", [RANK_FUSION, SCORE_FUSION], ids=["rank", "score"])
def test_a_fusion_input_unions_cursor_is_mongods(rust_mongo: Any, mongod: Any, stage: dict) -> None:
    """mongod 8.2.11 explains the input pipeline a fusion's `$unionWith`
    runs as its own query: a `$cursor` over the stages the user wrote, then
    the fusion's. This server listed every stage desugared with no
    `$cursor` until RM-696."""
    want = _union_cursors(_fusion_explain(mongod, stage))
    assert want and _union_cursors(_fusion_explain(rust_mongo, stage)) == want


def _union_counts(reply: dict) -> list:
    return [
        [s.get("nReturned") for s in stage["$unionWith"]["pipeline"][1:]]
        for stage in reply["stages"]
        if "$unionWith" in stage
    ]


@pytest.mark.backlog("RM-830")
def test_a_fusion_input_unions_stages_count_as_mongod_does(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counts each stage after a fusion input `$unionWith`'s
    `$cursor` (`nReturned` 4 after a `$limit: 4`); this server lists them
    with no counts."""
    want = _union_counts(_fusion_explain(mongod, RANK_FUSION, "executionStats"))
    assert _union_counts(_fusion_explain(rust_mongo, RANK_FUSION, "executionStats")) == want


def _fusion_union_head(client: Any) -> list:
    reply = _fusion_explain(
        client,
        {
            "$rankFusion": {
                "input": {
                    "pipelines": {
                        "a": [{"$sort": {"x": 1}}],
                        "b": [{"$sort": {"x": -1}}],
                    }
                }
            }
        },
    )
    heads = []
    for stage in reply["stages"]:
        if "$unionWith" in stage:
            heads.append(next(iter(stage["$unionWith"]["pipeline"][0])))
    return heads


@pytest.mark.backlog_fixed("RM-696")
def test_a_fusion_input_union_opens_with_its_cursor(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains the `$unionWith` a `$rankFusion` input
    pipeline runs in as any other: its pipeline opens with a `$cursor` of
    its own. This server listed the desugared stages with no `$cursor`."""
    heads = _fusion_union_head(mongod)
    assert heads and _fusion_union_head(rust_mongo) == heads
