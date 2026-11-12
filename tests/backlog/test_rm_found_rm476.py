"""Divergences found while working RM-476 (2026-11-12), each measured
against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.test_rm_found_rm375 import _KEYS, _ints


def _chain(stats: Any) -> list:
    """A plan's stages from the top, a SORT with its `type`."""
    out = []
    while isinstance(stats, dict):
        stage = stats.get("stage")
        out.append(f"SORT {stats.get('type')}" if stage == "SORT" else stage)
        stats = stats.get("inputStage")
    return out


def _or_sorted(client: Any) -> tuple:
    db = client["rm_found_rm476_or"]
    db.drop_collection("c")
    db.c.insert_many(_ints())
    db.c.create_index(_KEYS)
    try:
        e = db.command(
            "explain",
            {"find": "c", "filter": {"$or": [{"y": 0}, {"y": {"$gt": 1}}]}, "sort": {"x": 1}},
            verbosity="executionStats",
        )
    finally:
        client.drop_database("rm_found_rm476_or")
    plan = e["queryPlanner"]["winningPlan"]
    sort = e["executionStats"]["executionStages"]
    while sort.get("stage") != "SORT":
        sort = sort["inputStage"]
    return (
        _chain(plan.get("queryPlan", plan)),
        sort.get("totalDataSizeSorted"),
        e["executionStats"]["nReturned"],
    )


@pytest.mark.backlog("RM-557")
def test_an_or_of_one_index_sorts_its_keys(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers `find({$or: [{y: 0}, {y: {$gt: 1}}]}).sort({x:
    1})` over `{y: 1, x: 1}` with one two-interval IXSCAN whose keys it
    sorts below the FETCH (`SUBPLAN <- FETCH <- SORT default <- IXSCAN`,
    520 counted for eight members). This server sorts the fetched
    documents above the FETCH (`SORT simple`, 480)."""
    answers = {"mongod": _or_sorted(mongod), "rust": _or_sorted(rust_mongo)}
    assert answers["mongod"] == (["SUBPLAN", "FETCH", "SORT default", "IXSCAN"], 520, 8)
    assert answers["rust"] == answers["mongod"]
