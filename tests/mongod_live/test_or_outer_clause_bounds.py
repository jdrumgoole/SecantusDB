"""A clause beside an `$or`, decided by every branch's bounds, against mongod 8.2 (RM-574).

mongod pushes a clause beside a contained `$or` into each branch's bounds;
when every branch's bounds decide it, nothing rechecks it. One a branch
cannot bound (its index lacks the field) is still rechecked in a FETCH
above the `OR`. Each case compares the plan's stage chain and the rows.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

YX = [("y", 1), ("x", 1)]
W = [("w", 1)]
OUT = {"_id": 0, "x": 1}

CASES: dict[str, tuple[dict, list]] = {
    "decided by every branch": (
        {"$or": [{"y": 0}, {"y": 1, "x": {"$gt": 3}}], "x": {"$lt": 10}},
        [YX],
    ),
    "a branch without the field": (
        {"$or": [{"y": 0}, {"w": 1}], "x": {"$lt": 10}},
        [YX, W],
    ),
    "a field no index holds": (
        {"$or": [{"y": 0}, {"y": 1, "x": {"$gt": 3}}], "z": 1},
        [YX],
    ),
}


def _chain(plan: Any) -> list:
    out = []
    while isinstance(plan, dict):
        out.append((plan.get("stage"), plan.get("filter")))
        plan = plan.get("inputStage") or (plan.get("inputStages") or [None])[0]
    return out


def _answer(client: Any, query: dict, indexes: list) -> tuple:
    db = client["rm574_" + uuid.uuid4().hex[:8]]
    try:
        db.c.insert_many(
            [{"_id": n, "x": n, "y": n % 3, "z": n % 2, "w": n % 4} for n in range(12)]
        )
        for index in indexes:
            db.c.create_index(index)
        command = {"find": "c", "filter": query, "projection": OUT}
        plan = db.command("explain", command)["queryPlanner"]["winningPlan"]
        rows = db.command(command)["cursor"]["firstBatch"]
        return _chain(plan), sorted(row["x"] for row in rows)
    finally:
        client.drop_database(db.name)


@pytest.mark.parametrize("case", sorted(CASES))
def test_a_clause_beside_an_or(case: str, rust_mongo: Any, mongod: Any) -> None:
    query, indexes = CASES[case]
    assert _answer(rust_mongo, query, indexes) == _answer(mongod, query, indexes)
