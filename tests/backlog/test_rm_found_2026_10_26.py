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

from tests.backlog.conftest import PLATFORM, mongo_answer, only_on

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
    `spilledDataStorageSize` are mongod's to the byte -- each platform's
    mongod, whose sizes differ (the in-order sort: 130545 bytes on arm64,
    131252 on x86_64). On x86_64 the `after $project` case is RM-402's gap."""
    ours, theirs = _sort_storage_sizes(rust_mongo), _sort_storage_sizes(mongod)
    if PLATFORM == "x86_64":
        ours.pop("after $project")
        theirs.pop("after $project")
    assert ours == theirs


@pytest.mark.backlog("RM-402")
def test_x86_spill_after_project_storage_size(rust_mongo: Any, mongod: Any) -> None:
    """On x86_64 mongod 8.2 stores 122253 bytes for the 29 spills of a sort
    after `$project`; this server stores 122288 (CI, 2026-11-07). On arm64
    the two agree, pinned by RM-325's regression test."""
    only_on("x86_64", why="the arm64 sizes agree (RM-325's regression test)")
    ours = _sort_storage_sizes(rust_mongo)["after $project"]
    assert ours == _sort_storage_sizes(mongod)["after $project"]


def _group_storage_sizes(client: Any) -> dict:
    db = _spill_db(client)
    groups = {
        # The case RM-325 was filed on: one accumulator, its state an array.
        "$push": {"_id": "$n", "s": {"$push": "$s"}},
        # A state that is a document: a partial sum and a count, Welford's
        # state, the merged fields.
        "$avg": {"_id": "$n", "a": {"$avg": "$n"}},
        "$stdDevPop": {"_id": "$n", "d": {"$stdDevPop": "$n"}},
        "$mergeObjects": {"_id": "$n", "o": {"$mergeObjects": {"a": "$s"}}},
        # The missing value: mongod writes its type byte alone.
        "$first": {"_id": "$n", "f": {"$first": "$zz"}},
    }
    sizes = {}
    for name, group in groups.items():
        entry = _profiled(
            db,
            [{"$unwind": "$s"}, {"$group": group}],
            {"internalDocumentSourceGroupMaxMemoryBytes": 5_000},
            "rm325" + name,
        )
        sizes[name] = (entry.get("groupSpills"), entry.get("groupSpilledDataStorageSize"))
    return sizes


@pytest.mark.backlog_fixed("RM-325")
def test_group_spill_storage_size(rust_mongo: Any, mongod: Any) -> None:
    """A classic `$group` spilled its groups as this server's own BSON
    (320100 bytes for a `$push` of 300 1000-byte strings, where mongod 8.2
    stores 16321); it now writes mongod's sorter file -- each group's key
    and its accumulators' `getValue(toBeMerged)` states -- so
    `groupSpilledDataStorageSize` is mongod's to the byte."""
    assert _group_storage_sizes(rust_mongo) == _group_storage_sizes(mongod)


def _group_held_value_spills(client: Any) -> dict:
    db = _spill_db(client)
    groups = {
        "$first": {"_id": "$n", "f": {"$first": "$s"}},
        "$last": {"_id": "$n", "f": {"$last": "$s"}},
        "$firstN": {"_id": "$n", "f": {"$firstN": {"n": 2, "input": "$s"}}},
        "$maxN": {"_id": "$n", "f": {"$maxN": {"n": 2, "input": "$s"}}},
        "$topN": {"_id": "$n", "f": {"$topN": {"n": 2, "sortBy": {"n": -1}, "output": "$s"}}},
        "object _id": {"_id": {"n": "$n"}, "f": {"$first": "$zz"}},
    }
    spills = {}
    for name, group in groups.items():
        entry = _profiled(
            db,
            [{"$unwind": "$s"}, {"$group": group}],
            {"internalDocumentSourceGroupMaxMemoryBytes": 5_000},
            "rm502" + name,
        )
        spills[name] = (entry.get("groupSpills"), entry.get("groupSpilledBytes"))
    return spills


@pytest.mark.backlog("RM-502")
def test_group_held_value_spills(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2 counts the 1000-byte string a classic `$first` holds, and
    spills 60 times as for a `$push` (this server: 9); it counts an object
    `_id` as the key it groups on (9 spills; this server 15)."""
    assert _group_held_value_spills(rust_mongo) == _group_held_value_spills(mongod)


def _window_storage_size(client: Any) -> int:
    db = _spill_db(client)

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
    return window_storage() - before


@pytest.mark.backlog("RM-325")
def test_spill_storage_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2 spills a `$setWindowFields` to a WiredTiger table (4096
    bytes here); this server writes a file of its own (310800)."""
    assert _window_storage_size(rust_mongo) == _window_storage_size(mongod)


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
    """mongod 8.2 on arm64 counts a `$group` of `$sum` and `$count` as
    115200 bytes over its 22 spills; this server counts 110400. On x86_64
    mongod and this server agree (CI, 2026-11-07)."""
    only_on("arm64", why="x86_64 mongod and this server agree")
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


@pytest.mark.backlog_fixed("RM-326")
def test_sort_profile_per_batch(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 profiled the sort in front of a `$setWindowFields` once
    per cursor-stage batch: `sortSpills: 52` for a sort that spilled 13, and
    `spillToDisk` counted the 20 chunks its runs were written as."""
    assert _per_batch(rust_mongo) == _per_batch(mongod)


# ---------------------------------------- RM-327 window budget residuals --

WINDOWS = {
    "[-5, 5] $sum": ({"$sum": "$n", "window": {"documents": [-5, 5]}}, False),
    "$rank": ({"$rank": {}}, False),
    "whole partitions": ({"$sum": "$n"}, True),
    "[-5, 5] $push of strings": ({"$push": "$s", "window": {"documents": [-5, 5]}}, False),
    "whole partitions $avg": ({"$avg": "$n"}, True),
    "partitioned $rank": ({"$rank": {}}, True),
    "partitioned $shift back": ({"$shift": {"output": "$n", "by": -1}}, True),
}

#: The cases whose least budget macOS mongod counts with its own function
#: sizes (RM-403): this server counts Linux's, the exemplar CI measures.
MACOS_SIZES = ("[-5, 5] $sum", "whole partitions", "[-5, 5] $push of strings")


def _least_budgets(client: Any, names: Any = WINDOWS) -> dict:
    client.drop_database("rm318")
    coll = client.rm318.c
    coll.insert_many([{"_id": i, "k": i % 7, "n": i * 2, "s": "x" * (300 + i)} for i in range(100)])
    coll.create_index("n")
    out = {}
    for name in names:
        output, partitioned = WINDOWS[name]
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


@pytest.mark.backlog_fixed("RM-327")
def test_window_budget_residuals(rust_mongo: Any, mongod: Any) -> None:
    """The least `$setWindowFields` budget each window runs under was a few
    bytes off mongod's; it is now Linux mongod 8.2.12's to the byte. A rank
    function's cache holds whole documents carrying `$sort`'s sort key (two
    at its peak: 1619 bytes) and the function its accumulator; a
    partitioned window counts the partition key and the next partition's
    first document its iterator read to find the end (2304). On arm64 --
    this Mac -- the cases macOS mongod sizes differently are RM-403's
    gap, so only the others are compared."""
    names = list(WINDOWS)
    if PLATFORM == "arm64":
        names = [n for n in names if n not in MACOS_SIZES]
    assert _least_budgets(rust_mongo, names) == _least_budgets(mongod, names)


@pytest.mark.backlog("RM-403")
def test_macos_window_function_sizes(rust_mongo: Any, mongod: Any) -> None:
    """macOS mongod 8.2.1 counts some window functions at its own sizes --
    a growing `$sum` 184 bytes, not Linux's 176, a removable `$push` 32
    less -- so these windows' least budgets are 1230, 2312 and 12853 on
    this Mac (1222, 2304 and 12885 on Linux)."""
    only_on("arm64", why="Linux mongod's sizes are this server's (RM-327's regression test)")
    assert _least_budgets(rust_mongo, MACOS_SIZES) == _least_budgets(mongod, MACOS_SIZES)


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


@pytest.mark.backlog_fixed("RM-328")
def test_explain_memory_counters(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `explain` listed a `$group`'s accumulator memory and
    output size, and the `$_internalSetWindowFields` stage it runs with
    its memory counters, which this server did not (fixed 2026-11-08; the
    values: `tests/mongod_live/test_explain_memory_counters.py`)."""
    assert _explained(rust_mongo) == _explained(mongod)
