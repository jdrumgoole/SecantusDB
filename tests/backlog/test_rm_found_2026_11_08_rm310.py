"""Divergences found while listing the DISTINCT_SCANs a `$group` tried
(RM-310, 2026-11-08), each measured against mongod 8.2.11."""

from __future__ import annotations

import uuid
from typing import Any

import pytest

_COUNTERS = ("works", "advanced", "needTime", "needYield", "saveState", "restoreState", "isEOF")


def _coll(client: Any) -> Any:
    c = client["rm_found_1108_310"]["c" + uuid.uuid4().hex[:10]]
    c.insert_many([{"_id": i, "x": i % 4, "y": i, "z": -i} for i in range(1, 25)])
    return c


def _counters(stage: dict) -> list[dict]:
    """Each stage's work counters, top down."""
    out = []
    while stage:
        out.append({k: stage.get(k) for k in ("stage", *_COUNTERS)})
        stage = stage.get("inputStage")
    return out


def _distinct_scan_runs(client: Any) -> dict:
    c = _coll(client)
    c.create_index("x")
    db = c.database
    d = db.command(
        "explain",
        {"distinct": c.name, "key": "x", "query": {"x": {"$gt": 0}}},
        verbosity="executionStats",
    )
    g = db.command(
        "explain",
        {
            "aggregate": c.name,
            "pipeline": [{"$group": {"_id": "$x", "f": {"$first": "$z"}}}],
            "cursor": {},
        },
        verbosity="executionStats",
    )
    return {
        "distinct": _counters(d["executionStats"]["executionStages"]),
        "group": _counters(g["stages"][0]["$cursor"]["executionStats"]["executionStages"]),
    }


@pytest.mark.backlog("RM-446")
def test_a_distinct_scan_run_reports_its_work_counters(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `executionStats` of a DISTINCT_SCAN plan, for
    `distinct` and for a `$group` it answers from one, gives every stage's
    `works`, `advanced`, `needTime`, `needYield`, `saveState`,
    `restoreState` and `isEOF`. This server's gives none of them."""
    got, want = _distinct_scan_runs(rust_mongo), _distinct_scan_runs(mongod)
    assert want["distinct"][1]["works"] == 4
    assert got == want


def _trailing_bound(client: Any) -> tuple:
    c = _coll(client)
    c.create_index([("x", 1), ("y", 1)])
    e = c.database.command(
        "explain",
        {"distinct": c.name, "key": "x", "query": {"x": {"$gt": 0}, "y": {"$gt": 3}}},
        verbosity="executionStats",
    )
    stats = e["executionStats"]
    return stats["nReturned"], stats["totalKeysExamined"], stats["totalDocsExamined"]


@pytest.mark.backlog("RM-447")
def test_a_distinct_scan_bounded_on_a_trailing_field_examines_mongods_keys(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's DISTINCT_SCAN of `x_1_y_1` for `distinct("x", {x:
    {$gt: 0}, y: {$gt: 3}})` over `x = i % 4, y = i` (i 1 to 24) returns
    three keys and examines six (eight works). This server's examines
    three. Perhaps the same seek as RM-377's key past the bounds, once per
    value of `x`: not confirmed in the source."""
    got, want = _trailing_bound(rust_mongo), _trailing_bound(mongod)
    assert want == (3, 6, 0)
    assert got == want
