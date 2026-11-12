"""Divergences found while making a sort that keeps its members' metadata a
`default` sort (RM-472, 2026-11-12), each measured against mongod 8.2.11."""

from __future__ import annotations

import uuid
from typing import Any

import pytest


def _explain(client: Any, seed: Any, command: dict, verbosity: str) -> dict:
    db = client["rm_found_472_" + uuid.uuid4().hex[:8]]
    seed(db)
    try:
        return db.command("explain", command, verbosity=verbosity)
    finally:
        client.drop_database(db.name)


def _sort_stage(stats: Any) -> dict:
    while isinstance(stats, dict):
        if stats.get("stage") == "SORT":
            return stats
        stats = stats.get("inputStage")
    raise AssertionError("no SORT stage")


def _stages(plan: Any) -> list:
    out = []
    while isinstance(plan, dict):
        out.append(plan.get("stage"))
        plan = plan.get("inputStage") or (plan.get("inputStages") or [None])[0]
    return out


def _seed_documents(db: Any) -> None:
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(12)])
    db.c.create_index([("y", 1), ("x", 1)])


@pytest.mark.backlog("RM-577")
def test_a_default_sort_of_documents_counts_its_members(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `default` SORT of a `showRecordId` read counts each
    working-set member it sorts (`totalDataSizeSorted`): 1122 for six
    documents over a COLLSCAN, 374 for two under a `limit`, 2244 for twelve
    over a FETCH. Without `showRecordId` the same read sorts `simple` and
    counts 402. This server counts the documents, 402, both ways."""
    commands = {
        "collscan": {"find": "c", "filter": {"z": 0}, "sort": {"z": 1}, "showRecordId": True},
        "limit": {
            "find": "c",
            "filter": {"z": 0},
            "sort": {"z": 1},
            "limit": 2,
            "showRecordId": True,
        },
        "fetch": {
            "find": "c",
            "filter": {"y": {"$gte": 0}},
            "sort": {"z": 1},
            "showRecordId": True,
        },
        "simple": {"find": "c", "filter": {"z": 0}, "sort": {"z": 1}},
    }
    answers = {
        server: {
            name: _sort_stage(
                _explain(client, _seed_documents, command, "executionStats")["executionStats"][
                    "executionStages"
                ]
            ).get("totalDataSizeSorted")
            for name, command in commands.items()
        }
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == {"collscan": 1122, "limit": 374, "fetch": 2244, "simple": 402}
    assert answers["rust"] == answers["mongod"]


def _seed_points(db: Any) -> None:
    db.c.insert_many([{"_id": n, "z": n % 2, "loc": [n, n]} for n in range(12)])
    db.c.create_index([("loc", "2d")])


@pytest.mark.backlog("RM-578")
def test_a_near_read_explains_as_geo_near_2d(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 plans a `$near` read over a `2d` index as `GEO_NEAR_2D`
    (`SORT <- GEO_NEAR_2D` under a `sort`). This server's `explain` names a
    COLLSCAN."""
    commands = {
        "near": {"find": "c", "filter": {"loc": {"$near": [0, 0]}}},
        "sorted": {"find": "c", "filter": {"loc": {"$near": [0, 0]}}, "sort": {"z": 1}},
    }
    answers = {
        server: {
            name: _stages(
                _explain(client, _seed_points, command, "queryPlanner")["queryPlanner"][
                    "winningPlan"
                ]
            )
            for name, command in commands.items()
        }
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == {"near": ["GEO_NEAR_2D"], "sorted": ["SORT", "GEO_NEAR_2D"]}
    assert answers["rust"] == answers["mongod"]
