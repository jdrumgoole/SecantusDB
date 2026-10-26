"""Gap tests for what the aggregation memory budgets left open (RM-211,
RM-315), found 2026-10-26 by comparing the spill counters, the budgets'
boundaries and `explain` with mongod 8.2.11.

Every test lowers a budget on both servers (`setParameter`), runs the same
work over small documents and compares the answers mongod gives -- so it
fails while the item is open, on an assertion.
"""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

DEFAULT = 104_857_600


@contextmanager
def knob(client: Any, name: str, value: int) -> Iterator[None]:
    client.admin.command({"setParameter": 1, name: value})
    try:
        yield
    finally:
        client.admin.command({"setParameter": 1, name: DEFAULT})


def _spill_db(client: Any) -> Any:
    client.drop_database("rmspill")
    db = client.rmspill
    db.c.insert_many([{"_id": i, "k": i % 7, "s": "x" * 1000, "n": i} for i in range(300)])
    return db


def _profiled(db: Any, pipeline: list, knobs: dict, comment: str) -> dict:
    client = db.client
    for name, value in knobs.items():
        client.admin.command({"setParameter": 1, name: value})
    db.command({"profile": 2})
    try:
        list(db.c.aggregate(pipeline, comment=comment))
    finally:
        db.command({"profile": 0})
        for name in knobs:
            client.admin.command({"setParameter": 1, name: DEFAULT})
    return db.system.profile.find_one({"command.comment": comment}) or {}


# ------------------------------------------- RM-316 spill storage size --


def _storage_sizes(client: Any) -> dict:
    db = _spill_db(client)
    sort = _profiled(
        db,
        [{"$sort": {"s": 1, "n": -1}}, {"$project": {"_id": 1}}],
        {"internalQueryMaxBlockingSortMemoryUsageBytes": 50_000},
        "rm316sort",
    )
    group = _profiled(
        db,
        [{"$unwind": "$s"}, {"$group": {"_id": "$n", "s": {"$push": "$s"}}}],
        {"internalDocumentSourceGroupMaxMemoryBytes": 5_000},
        "rm316group",
    )
    return {
        "sort": sort.get("sortSpilledDataStorageSize"),
        "group": group.get("groupSpilledDataStorageSize"),
    }


@pytest.mark.backlog("RM-316")
def test_spill_storage_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 compresses a sort's runs (130545 bytes for 2461200) and
    spills a `$group` to a WiredTiger table."""
    assert _storage_sizes(rust_mongo) == _storage_sizes(mongod)


# --------------------------------------- RM-317 sort stats per batch --


def _per_batch(client: Any) -> dict:
    db = _spill_db(client)

    def spill_to_disk() -> int:
        return client.admin.command("serverStatus")["metrics"]["query"]["sort"]["spillToDisk"]

    before = spill_to_disk()
    entry = _profiled(
        db,
        [{"$setWindowFields": {"sortBy": {"n": 1}, "output": {"t": {"$sum": "$n"}}}}],
        {"internalQueryMaxBlockingSortMemoryUsageBytes": 50_000},
        "rm317",
    )
    return {
        "sortSpills": entry.get("sortSpills"),
        "sortSpilledRecords": entry.get("sortSpilledRecords"),
        "spillToDisk": spill_to_disk() - before,
    }


@pytest.mark.backlog("RM-317")
def test_sort_profile_per_batch(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 profiles the sort in front of a `$setWindowFields` once
    per cursor-stage batch: `sortSpills: 52` for a sort that spilled 13."""
    assert _per_batch(rust_mongo) == _per_batch(mongod)


# ---------------------------------------- RM-318 window budget residuals --

WINDOWS = {
    "[-5, 5] $sum": ({"$sum": "$n", "window": {"documents": [-5, 5]}}, False),
    "$rank": ({"$rank": {}}, False),
    "whole partitions": ({"$sum": "$n"}, True),
    "[-5, 5] $push of strings": ({"$push": "$s", "window": {"documents": [-5, 5]}}, False),
}


def _least_budgets(client: Any) -> dict:
    client.drop_database("rm318")
    coll = client.rm318.c
    coll.insert_many([{"_id": i, "k": i % 7, "n": i * 2, "s": "x" * (300 + i)} for i in range(100)])
    coll.create_index("n")
    out = {}
    for name, (output, partitioned) in WINDOWS.items():
        spec: dict = {"sortBy": {"n": 1}, "output": {"t": output}}
        if partitioned:
            spec["partitionBy"] = "$k"
        pipeline = [{"$setWindowFields": spec}, {"$project": {"_id": 1, "t": 1}}]

        def runs(budget: int, pipeline: list = pipeline) -> bool:
            with knob(client, "internalDocumentSourceSetWindowFieldsMaxMemoryBytes", budget):
                got = mongo_answer(lambda: list(coll.aggregate(pipeline, allowDiskUse=False)))
            return got[0] == "ok"

        lo, hi = 1, 400_000
        while hi - lo > 1:
            mid = (lo + hi) // 2
            if runs(mid):
                hi = mid
            else:
                lo = mid
        out[name] = hi
    return out


@pytest.mark.backlog("RM-318")
def test_window_budget_residuals(rust_mongo: Any, mongod: Any) -> None:
    """The least `$setWindowFields` budget each window runs under, mongod
    8.2.11's."""
    assert _least_budgets(rust_mongo) == _least_budgets(mongod)


# ------------------------------------------- RM-319 explain's counters --


def _explained(client: Any) -> dict:
    db = _spill_db(client)
    out = {}
    for name, pipeline in {
        "$group": [{"$unwind": "$s"}, {"$group": {"_id": "$k", "n": {"$sum": 1}}}],
        "$setWindowFields": [
            {"$unwind": "$s"},
            {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"t": {"$sum": "$n"}}}},
        ],
    }.items():
        ex = db.command(
            {
                "explain": {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
                "verbosity": "executionStats",
            }
        )
        out[name] = [sorted(stage) for stage in ex.get("stages", [])[1:]]
    return out


@pytest.mark.backlog("RM-319")
def test_explain_memory_counters(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `explain` lists a `$group`'s accumulator memory and
    output size, and the `$_internalSetWindowFields` stage it runs with
    its memory counters."""
    assert _explained(rust_mongo) == _explained(mongod)
