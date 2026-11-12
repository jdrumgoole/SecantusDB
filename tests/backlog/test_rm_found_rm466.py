"""Divergences found while fixing the explain of `$planCacheStats` (RM-466,
2026-11-12), each measured against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

_P = {"$planCacheStats": {}}


def _entries(client: Any, pipeline: list) -> dict:
    """How many plan cache entries `pipeline` returns, out of how many."""
    db = client["rm_found_rm466"]
    client.drop_database("rm_found_rm466")
    try:
        db.c.insert_many([{"x": i % 3, "y": i % 5, "z": i} for i in range(50)])
        for key in ("x", "y", "z"):
            db.c.create_index(key)
        for q in ({"x": 1, "y": 1}, {"x": 2, "z": {"$gt": 3}}, {"y": 1, "z": {"$lt": 40}}):
            for _ in range(3):
                list(db.c.find(q))
        return {
            "all": len(list(db.c.aggregate([_P]))),
            "returned": len(list(db.c.aggregate(pipeline))),
        }
    finally:
        client.drop_database("rm_found_rm466")


@pytest.mark.backlog("RM-554")
def test_a_later_match_replaces_the_one_plan_cache_stats_absorbed(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's `$planCacheStats` absorbs the `$match` right after it,
    and then absorbs a later `$match` that a `$project` swaps ahead of
    itself in place of the first, whose filter is lost: every entry comes
    back, inactive or not. This server's stage keeps the first `$match` and
    runs the second after the `$project`, so none of the (active) entries
    passes `isActive: false`."""
    pipeline = [
        _P,
        {"$match": {"isActive": False}},
        {"$project": {"isActive": 1, "works": 1}},
        {"$match": {"works": {"$gte": 0}}},
    ]
    answers = {"mongod": _entries(mongod, pipeline), "rust": _entries(rust_mongo, pipeline)}
    assert answers["mongod"]["all"] > 0
    assert answers["mongod"]["returned"] == answers["mongod"]["all"]
    assert answers["rust"] == answers["mongod"]
