"""Gap tests for what working RM-311 (a `$match` on a `$group`'s key) found
on 2026-11-09, comparing with mongod 8.2.11.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

_G = {"$group": {"_id": "$x", "f": {"$first": "$y"}}}

_LISTED = {
    "after_a_group": [_G, {"$match": {"_id": {"$exists": True}, "f": 1}}],
    "expr_after_a_group": [_G, {"$match": {"$expr": {"$eq": ["$f", 2]}}}],
    "after_a_limit": [{"$limit": 3}, {"$match": {"x": {"$exists": True}, "y": 1}}],
    "expr_after_a_limit": [{"$limit": 3}, {"$match": {"$expr": {"$eq": ["$y", 2]}}}],
}


def _listed_matches(client: Any) -> dict:
    c = client["rm_found_1109"]["c" + uuid.uuid4().hex[:10]]
    c.insert_many([{"_id": i, "x": i % 3, "y": i} for i in range(1, 8)])
    out = {}
    for name, pipeline in _LISTED.items():
        e = c.database.command("aggregate", c.name, pipeline=pipeline, explain=True)
        out[name] = [s for s in e.get("stages", [])[1:] if "$match" in s]
    return out


@pytest.mark.backlog("RM-444")
def test_explain_lists_a_match_after_the_cursor_in_written_order(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 lists a `$match` stage after the cursor with its
    clauses as written (`{x: {$exists: true}, y: 1}` is `$exists` then
    `$eq`), and an `$expr`'s `$_internalExprEq` ahead of the `$expr`; it
    sorts only the query's `parsedQuery`."""
    got, want = _listed_matches(rust_mongo), _listed_matches(mongod)
    assert want["after_a_limit"] == [
        {"$match": {"$and": [{"x": {"$exists": True}}, {"y": {"$eq": 1}}]}}
    ]
    assert got == want


def _projection_stage(client: Any) -> str:
    c = client["rm_found_1109"]["c" + uuid.uuid4().hex[:10]]
    c.insert_many([{"_id": i, "x": i % 3} for i in range(1, 8)])
    e = c.database.command("aggregate", c.name, pipeline=[{"$project": {"w": "$x"}}], explain=True)
    planner = e.get("queryPlanner") or e["stages"][0]["$cursor"]["queryPlanner"]
    return planner["winningPlan"]["stage"]


@pytest.mark.backlog_fixed("RM-445")
def test_a_renaming_projection_is_projection_default(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains `[{$project: {w: "$x"}}]`'s projection as
    `PROJECTION_DEFAULT`: a field computed from a path is no inclusion, so
    the projection is not a simple one. This server called it
    `PROJECTION_SIMPLE` until 2026-11-09 (fixed with RM-373)."""
    want = _projection_stage(mongod)
    assert want == "PROJECTION_DEFAULT"
    assert _projection_stage(rust_mongo) == want
