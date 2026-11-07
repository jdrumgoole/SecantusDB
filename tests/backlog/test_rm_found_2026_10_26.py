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


# ------------------------------------------- RM-325 spill storage size --


def _bucket_auto_storage(client: Any, db: Any) -> dict:
    def counters() -> dict:
        return client.admin.command("serverStatus")["metrics"]["query"]["bucketAuto"]

    before = counters()
    _profiled(
        db,
        [{"$bucketAuto": {"groupBy": "$n", "buckets": 5, "output": {"s": {"$push": "$s"}}}}],
        {"internalDocumentSourceBucketAutoMaxMemoryBytes": 50_000},
        "rm325bucket",
    )
    after = counters()
    return {k: after[k] - before[k] for k in after}


def _sort_storage_sizes(client: Any) -> dict:
    db = _spill_db(client)
    sizes = {}
    pipelines = {
        # The query layer's sort, over documents that arrive in its order:
        # a `Value` key (an array of two) and the document's BSON.
        "in order": [{"$sort": {"n": 1, "s": 1}}],
        # A pipeline `$sort`: the fields kept, as a `Document`.
        "after $project": [{"$project": {"s": 1, "k": 1}}, {"$sort": {"k": -1, "s": 1}}],
    }
    for name, pipeline in pipelines.items():
        entry = _profiled(
            db, pipeline, {"internalQueryMaxBlockingSortMemoryUsageBytes": 50_000}, "rm325" + name
        )
        sizes[name] = (entry.get("sortSpills"), entry.get("sortSpilledDataStorageSize"))
    sizes["$bucketAuto"] = _bucket_auto_storage(client, db)
    return sizes


@pytest.mark.backlog_fixed("RM-325")
def test_spill_file_storage_size(rust_mongo: Any, mongod: Any) -> None:
    """A sorter's spill file was this server's own; it is now mongod 8.2's:
    64 KiB chunks of `serializeForSorter` records, each snappy-compressed by
    mongod's compressor, so a sort's and `$bucketAuto`'s
    `spilledDataStorageSize` are mongod's to the byte."""
    assert _sort_storage_sizes(rust_mongo) == _sort_storage_sizes(mongod)


def _storage_sizes(client: Any) -> dict:
    db = _spill_db(client)
    group = _profiled(
        db,
        [{"$unwind": "$s"}, {"$group": {"_id": "$n", "s": {"$push": "$s"}}}],
        {"internalDocumentSourceGroupMaxMemoryBytes": 5_000},
        "rm316group",
    )

    def window_storage() -> int:
        return client.admin.command("serverStatus")["metrics"]["query"]["setWindowFields"][
            "spilledDataStorageSize"
        ]

    before = window_storage()
    _profiled(
        db,
        [{"$setWindowFields": {"partitionBy": "$k", "output": {"t": {"$push": "$s"}}}}],
        {"internalDocumentSourceSetWindowFieldsMaxMemoryBytes": 50_000},
        "rm325window",
    )
    return {
        "group": group.get("groupSpilledDataStorageSize"),
        "window": window_storage() - before,
    }


@pytest.mark.backlog("RM-325")
def test_spill_storage_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2 spills a classic `$group` to a sorter file of its
    accumulators' merge values (16321 bytes here) and a `$setWindowFields`
    to a WiredTiger table (4096)."""
    assert _storage_sizes(rust_mongo) == _storage_sizes(mongod)


def _leading_sort_storage(client: Any) -> Any:
    db = _spill_db(client)
    entry = _profiled(
        db,
        [{"$sort": {"s": 1, "n": -1}}, {"$project": {"_id": 1}}],
        {"internalQueryMaxBlockingSortMemoryUsageBytes": 50_000},
        "rm316sort",
    )
    return entry.get("sortSpilledDataStorageSize")


@pytest.mark.backlog("RM-395")
def test_leading_sort_spill_storage_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2's `SORT` takes documents as its collection scan reads them,
    so a sort that reverses that order spills runs of the first documents
    read, each sorted (130545 bytes)."""
    assert _leading_sort_storage(rust_mongo) == _leading_sort_storage(mongod)


def _added_fields_sort(client: Any) -> Any:
    client.drop_database("rm396")
    coll = client.rm396.c
    coll.insert_many(
        [
            {
                "_id": i,
                "k": i % 7,
                "s": f"name-{i * 37 % 1000:04d}" * 20,
                "a": [i, i + 1],
                "o": {"x": i, "y": "z" * (i % 30)},
                "n": i,
            }
            for i in range(400)
        ]
    )
    entry = _profiled(
        client.rm396,
        [{"$addFields": {"t": {"$multiply": ["$n", 2]}}}, {"$sort": {"t": -1}}],
        {"internalQueryMaxBlockingSortMemoryUsageBytes": 60_000},
        "rm396",
    )
    return (entry.get("sortSpills"), entry.get("sortSpilledDataStorageSize"))


@pytest.mark.backlog("RM-396")
def test_added_fields_sort_spill_storage_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2 spills a `$sort` after `$addFields` at its own record
    boundaries: its first two runs of documents of varying size hold about
    three records more than this server's, so the merged runs (and the
    file, 39691 bytes) differ though the spills and bytes agree."""
    assert _added_fields_sort(rust_mongo) == _added_fields_sort(mongod)


def _group_sum_spilled_bytes(client: Any) -> Any:
    db = _spill_db(client)
    entry = _profiled(
        db,
        [{"$unwind": "$s"}, {"$group": {"_id": "$n", "t": {"$sum": "$n"}, "c": {"$count": {}}}}],
        {"internalDocumentSourceGroupMaxMemoryBytes": 5_000},
        "rm397",
    )
    return (entry.get("groupSpills"), entry.get("groupSpilledBytes"))


@pytest.mark.backlog("RM-397")
def test_group_sum_spilled_bytes(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2 counts a `$group` of `$sum` and `$count` as 115200 bytes
    over its 22 spills; this server counts 110400."""
    assert _group_sum_spilled_bytes(rust_mongo) == _group_sum_spilled_bytes(mongod)


# --------------------------------------- RM-326 sort stats per batch --


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


@pytest.mark.backlog("RM-326")
def test_sort_profile_per_batch(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 profiles the sort in front of a `$setWindowFields` once
    per cursor-stage batch: `sortSpills: 52` for a sort that spilled 13."""
    assert _per_batch(rust_mongo) == _per_batch(mongod)


# ---------------------------------------- RM-327 window budget residuals --

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


@pytest.mark.backlog("RM-327")
def test_window_budget_residuals(rust_mongo: Any, mongod: Any) -> None:
    """The least `$setWindowFields` budget each window runs under, mongod
    8.2.11's."""
    assert _least_budgets(rust_mongo) == _least_budgets(mongod)


# ------------------------------------------- RM-328 explain's counters --


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


@pytest.mark.backlog("RM-328")
def test_explain_memory_counters(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `explain` lists a `$group`'s accumulator memory and
    output size, and the `$_internalSetWindowFields` stage it runs with
    its memory counters."""
    assert _explained(rust_mongo) == _explained(mongod)
