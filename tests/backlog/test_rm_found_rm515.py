"""Gap tests for what modelling a sub-document's storage (RM-515,
2026-11-13) found beside it, compared with a live mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

WINDOW = {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"z": {"$first": "$n"}}}}


def _array_sizes(client: Any) -> dict:
    client.drop_database("rmfoundrm515")
    db = client.rmfoundrm515
    db.c.insert_many(
        [{"_id": i, "t": [1, "z" * 30, {"q": 1, "y": "w" * 50}], "n": i} for i in range(200)]
    )
    out = {}
    for name, first in {
        "a field read inside an array's documents": {"$addFields": {"q": "$t.y"}},
        "a field set inside an array's documents": {"$set": {"t.w": 1}},
    }.items():
        ex = db.command(
            {
                "explain": {"aggregate": "c", "pipeline": [first, WINDOW], "cursor": {}},
                "verbosity": "executionStats",
            }
        )
        stages = ex["stages"][1:]
        out[name] = (
            stages[1]["totalDataSizeSortedBytesEstimate"],
            stages[2]["maxTotalMemoryUsageBytes"],
        )
    return out


@pytest.mark.backlog("RM-636")
def test_an_arrays_documents_a_stage_touched_are_sized_by_their_storage(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 gives each document in an array a cache of its own when
    a stage reads (`$t.y`) or writes (`t.w`) a field through the array, and
    the window's sort and cache count it: (236000, 259600) and (279200,
    302800) bytes over 200 documents."""
    assert _array_sizes(rust_mongo) == _array_sizes(mongod)
