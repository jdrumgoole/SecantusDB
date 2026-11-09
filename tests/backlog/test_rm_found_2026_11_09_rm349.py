"""Gap tests for the Rust MongoDB server items found on 2026-11-09 while
working RM-349.

Each test asks mongod 8.2.11 live and asserts the Rust server gives the same
answer, reduced to the stages of a plan.
"""

from __future__ import annotations

from typing import Any

import pytest


def _stages(stage: Any) -> list:
    out = []
    while isinstance(stage, dict):
        out.append(stage.get("stage"))
        stage = stage.get("inputStage") or (stage.get("inputStages") or [None])[0]
    return out


@pytest.mark.backlog("RM-459")
def test_a_rooted_or_of_tried_branches_has_no_fetch_over_its_or(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 plans `{$or: [{a: {$gte: 2}, b: {$gte: 3}}, {a: 0, b:
    0}]}` over `a_1` and `b_1` branch by branch, each by trial, and its
    winning plan is SUBPLAN, OR, then each branch's FETCH over its IXSCAN.
    This server's puts a FETCH between the SUBPLAN and the OR."""

    def run(client: Any) -> tuple:
        coll = client.rm349f.c
        coll.drop()
        coll.insert_many([{"_id": n, "a": n % 7, "b": n % 11} for n in range(600)])
        coll.create_index("a")
        coll.create_index("b")
        query = {"$or": [{"a": {"$gte": 2}, "b": {"$gte": 3}}, {"a": 0, "b": 0}]}
        explained = coll.find(query).explain()
        return (
            _stages(explained["queryPlanner"]["winningPlan"]),
            _stages(explained["executionStats"]["executionStages"]),
        )

    want = run(mongod)
    assert want[0] == ["SUBPLAN", "OR", "FETCH", "IXSCAN"]
    assert run(rust_mongo) == want
