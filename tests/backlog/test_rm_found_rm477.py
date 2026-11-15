"""Divergences found while working RM-477 (2026-11-12), each measured
against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import OS, PLATFORM

#: The four documents `tools/probes/find_order.py`'s step 100 shrinks to.
_DOCS = [
    {"_id": 17, "a": "b", "b": 2},
    {"_id": 14, "b": 1},
    {"_id": 13, "a": 7, "b": 1},
    {"_id": 3, "a": 5, "b": 0},
]


def _merged(client: Any) -> tuple[list, list]:
    db = client["rm_found_rm477_merge"]
    db.drop_collection("c")
    db.c.insert_many(_DOCS)
    db.c.create_index([("a", 1), ("b", 1)])
    query = {"a": {"$in": [7, "b", 5, None]}}
    plan = db.command("explain", {"find": "c", "filter": query, "sort": {"b": 1}})
    stages = []
    stats = plan["queryPlanner"]["winningPlan"]
    while isinstance(stats, dict):
        stages.append(stats["stage"])
        stats = stats.get("inputStage")
    ids = [d["_id"] for d in db.c.find(query).sort("b", 1)]
    client.drop_database("rm_found_rm477_merge")
    return stages, ids


@pytest.mark.backlog_fixed("RM-559")
def test_a_sort_merge_beside_a_null_scan_breaks_ties_as_mongod(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 answers `find({a: {$in: [7, "b", 5, null]}}).sort({b:
    1})` over `{a: 1, b: 1}` with a SORT_MERGE of four scans and returns 14
    (no `a`, from the null scan) before 13 (`a: 7`), which tie on `b`. This
    server merged the same scans and returned 13 first: its heap was
    libstdc++'s on every platform, and the macOS arm64 mongod this asks
    links libc++, whose `pop_heap` hands back the other of two equal
    children. A macOS arm64 build now uses libc++'s heap. Linux mongod
    (libstdc++) answers 13 first, as this server does there."""
    answers = {"mongod": _merged(mongod), "rust": _merged(rust_mongo)}
    libcxx = (OS, PLATFORM) == ("darwin", "arm64")
    want = [3, 14, 13, 17] if libcxx else [3, 13, 14, 17]
    assert answers["mongod"] == (["FETCH", "SORT_MERGE"], want)
    assert answers["rust"] == answers["mongod"]
