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


def _fusion_union_head(client: Any) -> list:
    client.drop_database("rm426f")
    db = client.rm426f
    db.d.insert_many([{"_id": n, "x": n} for n in range(1, 7)])
    reply = db.command(
        "explain",
        {
            "aggregate": "d",
            "pipeline": [
                {
                    "$rankFusion": {
                        "input": {
                            "pipelines": {
                                "a": [{"$sort": {"x": 1}}],
                                "b": [{"$sort": {"x": -1}}],
                            }
                        }
                    }
                }
            ],
            "cursor": {},
        },
        verbosity="queryPlanner",
    )
    heads = []
    for stage in reply["stages"]:
        if "$unionWith" in stage:
            heads.append(next(iter(stage["$unionWith"]["pipeline"][0])))
    return heads


@pytest.mark.backlog("RM-696")
def test_a_fusion_input_union_opens_with_its_cursor(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains the `$unionWith` a `$rankFusion` input
    pipeline runs in as any other: its pipeline opens with a `$cursor` of
    its own. This server lists the desugared stages with no `$cursor`."""
    heads = _fusion_union_head(mongod)
    assert heads and _fusion_union_head(rust_mongo) == heads
