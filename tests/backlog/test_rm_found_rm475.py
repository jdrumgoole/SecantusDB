"""Divergences found while working RM-475 (2026-11-12), each measured
against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.opdebug import logged


def _sort_stage(stats: Any) -> dict:
    while isinstance(stats, dict):
        if stats.get("stage") == "SORT":
            return stats
        stats = stats.get("inputStage")
    raise AssertionError("no SORT stage")


def _spilled_under_collation(client: Any, is_mongod: bool) -> dict:
    client.admin.command("setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=100000)
    try:

        def setup(d: Any) -> None:
            d.c.insert_many([{"_id": i, "x": f"s{i}", "y": i % 3} for i in range(2000)])
            d.c.create_index([("y", 1), ("x", 1)])

        def op(d: Any) -> None:
            d.command(
                "find",
                "c",
                filter={"y": {"$gte": 0}},
                sort={"x": 1},
                collation={"locale": "fr"},
                allowDiskUse=True,
                batchSize=3000,
            )

        entry = logged(client, is_mongod, setup, op)[1][0]
    finally:
        client.admin.command(
            "setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=104857600
        )
    stage = _sort_stage(entry.get("execStats"))
    return {k: stage.get(k) for k in ("type", "spills", "spilledRecords", "spilledBytes")}


@pytest.mark.backlog("RM-555")
def test_a_spilled_sort_of_index_keys_writes_collation_keys(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SORT of index keys under a collation writes each
    member's sort key as its collation key when it spills: 2000 `"s{i}"`
    strings under `{locale: "fr"}` and a 100000-byte budget spill 157780
    bytes. This server writes the strings themselves: 149780."""
    answers = {
        "mongod": _spilled_under_collation(mongod, True),
        "rust": _spilled_under_collation(rust_mongo, False),
    }
    assert answers["mongod"] == {
        "type": "default",
        "spills": 2,
        "spilledRecords": 2000,
        "spilledBytes": 157780,
    }
    assert answers["rust"] == answers["mongod"]
