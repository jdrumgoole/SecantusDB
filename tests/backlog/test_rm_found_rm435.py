"""Divergences found working RM-435 (2026-11-14), each measured against
mongod 8.2.11."""

from __future__ import annotations

import re
from typing import Any

import pytest


def _engine(client: Any, group: dict) -> Any:
    client.drop_database("rm435_found")
    col = client.rm435_found.c
    col.insert_many([{"a": i % 3, "b": i, "s": "x" * (i + 20)} for i in range(10)])
    pipeline = [{"$group": group}, {"$sort": {"_id": 1}}]
    cmd = {"aggregate": "c", "pipeline": pipeline, "cursor": {}}
    e = col.database.command("explain", cmd, verbosity="executionStats")
    plan = e["stages"][0]["$cursor"]["queryPlanner"]["winningPlan"]
    text = plan.get("slotBasedPlan", {}).get("stages", "")
    return (
        e.get("explainVersion"),
        [next(iter(s)) for s in e["stages"]],
        re.sub(r'@"[^"]*"', "@uuid", text),
    )


@pytest.mark.backlog_fixed("RM-693")
@pytest.mark.parametrize(
    "group",
    [
        {"_id": {"$mod": ["$b", 2]}},
        {"_id": {"a": "$a", "b": {"$mod": ["$b", 2]}}, "n": {"$sum": 1}},
        {"_id": "$a", "q": {"$push": {"s": "$s"}}},
        {"_id": "$a", "q": {"$addToSet": {"s": "$s"}}},
    ],
)
def test_a_group_mongod_pushes_into_sbe_is_pushed(
    rust_mongo: Any, mongod: Any, group: dict
) -> None:
    """mongod 8.2.11 runs each of these `$group`s in SBE (`explainVersion`
    "2", no `$group` among the `stages`); this server kept them classic,
    and now explains them in SBE with mongod's slot-based plan text."""
    assert _engine(mongod, group)[:2] == ("2", ["$cursor", "$sort"])
    assert _engine(rust_mongo, group) == _engine(mongod, group)
