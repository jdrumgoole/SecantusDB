"""Gap tests for the sort counters, found 2026-11-07 while closing part of
RM-326, by comparing the profile's sort counts and `serverStatus`'s
`metrics.query.sort` with mongod 8.2.11.

Each test lowers the sort budget on both servers (`setParameter`), runs the
same work over the same small documents and compares what mongod reports.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.test_rm_found_2026_10_26 import _profiled, _spill_db, knob

SORT_BUDGET = {"internalQueryMaxBlockingSortMemoryUsageBytes": 50_000}
FIELDS = ("sortSpills", "sortSpilledRecords", "sortSpilledBytes", "sortTotalDataSizeBytes")
ADD = {"$addFields": {"z": 1}}


def _sort_metrics(client: Any) -> dict:
    sort = client.admin.command("serverStatus")["metrics"]["query"]["sort"]
    return {k: int(sort[k]) for k in ("spillToDisk", "totalKeysSorted", "totalBytesSorted")}


def _find(tree: Any, wanted: Any) -> dict:
    """The first document in an explain's tree that `wanted` accepts."""
    if isinstance(tree, dict):
        if wanted(tree):
            return tree
        tree = list(tree.values())
    if isinstance(tree, list):
        for item in tree:
            found = _find(item, wanted)
            if found:
                return found
    return {}


def _counts(db: Any, pipelines: dict) -> dict:
    out = {}
    for name, pipeline in pipelines.items():
        entry = _profiled(db, pipeline, SORT_BUDGET, f"rm326-{name}")
        out[name] = {k: entry.get(k) for k in FIELDS}
    return out


# ---------------------------------- RM-326 sort counts per load --


def _per_load(client: Any) -> dict:
    db = _spill_db(client)
    out = _counts(
        db,
        {
            "first batch": [{"$sort": {"n": -1}}, ADD],
            "skip moved in": [{"$sort": {"n": -1}}, ADD, {"$skip": 150}],
            "read to the end": [
                {"$sort": {"n": -1}},
                ADD,
                {"$group": {"_id": "$k", "c": {"$sum": 1}}},
            ],
            "window to the end": [
                {
                    "$setWindowFields": {
                        "sortBy": {"n": 1},
                        "output": {
                            "t": {"$sum": "$n", "window": {"documents": ["current", "unbounded"]}}
                        },
                    }
                }
            ],
        },
    )
    # `explain` reports the sort's own counts, once.
    for name, k in SORT_BUDGET.items():
        client.admin.command({"setParameter": 1, name: k})
    try:
        explained = db.command(
            "explain",
            {"aggregate": "c", "pipeline": [{"$sort": {"n": -1}}, ADD], "cursor": {}},
            verbosity="executionStats",
        )
    finally:
        for name in SORT_BUDGET:
            client.admin.command({"setParameter": 1, name: 104_857_600})
    stats = _find(explained, lambda d: "executionStages" in d)
    sort = _find(stats, lambda d: d.get("stage") == "SORT")
    out["explain"] = {k: sort.get(k) for k in ("spills", "spilledRecords", "spilledBytes")}
    # `$bucketAuto`'s sorter writes chunks the sort counters count.
    db.c.drop()
    db.c.insert_many([{"_id": i, "s": "x" * 1000, "n": i} for i in range(300)])
    before = _sort_metrics(client)
    with knob(client, "internalDocumentSourceBucketAutoMaxMemoryBytes", 50_000):
        list(
            db.c.aggregate(
                [{"$bucketAuto": {"groupBy": "$n", "buckets": 5, "output": {"s": {"$push": "$s"}}}}]
            )
        )
    out["bucketAuto chunks"] = _sort_metrics(client)["spillToDisk"] - before["spillToDisk"]
    return out


@pytest.mark.backlog_fixed("RM-326")
def test_sort_counts_per_load(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counted a query-layer sort once per buffer its cursor
    stage loaded (39 for a sort that spilled 13 under a first batch of 101,
    52 when the stages read it to the end) -- `explain` gave the sort's own
    13 -- and `spillToDisk` once per chunk any sorter wrote, `$bucketAuto`'s
    included (32 for its 20 spills)."""
    assert _per_load(rust_mongo) == _per_load(mongod)


def _depends_on_documents(client: Any) -> dict:
    db = _spill_db(client)
    return _counts(
        db,
        {
            "$unwind": [{"$sort": {"n": -1}}, {"$unwind": "$s"}, {"$project": {"_id": 1}}],
            "$rank": [{"$setWindowFields": {"sortBy": {"n": 1}, "output": {"r": {"$rank": {}}}}}],
            "running total": [
                {
                    "$setWindowFields": {
                        "sortBy": {"n": 1},
                        "output": {
                            "t": {"$sum": "$n", "window": {"documents": ["unbounded", "current"]}}
                        },
                    }
                }
            ],
            "partitioned": [
                {
                    "$setWindowFields": {
                        "partitionBy": "$k",
                        "sortBy": {"n": 1},
                        "output": {"t": {"$sum": "$n"}},
                    }
                }
            ],
        },
    )


@pytest.mark.backlog_fixed("RM-326")
def test_sort_counts_per_load_through_stages_that_filter(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counted the sort per load behind an `$unwind` (39), a
    `$rank` or a running total (39), and a partitioned window (42): the
    loads follow what those stages read of their input for the batch."""
    assert _depends_on_documents(rust_mongo) == _depends_on_documents(mongod)


# ------------------------------- RM-400 getMore and the sort counters --


def _get_mores(client: Any) -> dict:
    db = _spill_db(client)
    out = {}
    for name, run in {
        "aggregate": lambda: list(
            db.c.aggregate(
                [{"$setWindowFields": {"sortBy": {"n": 1}, "output": {"t": {"$sum": "$n"}}}}],
                comment="rm397-aggregate",
            )
        ),
        "find": lambda: list(
            db.c.find({}, {"_id": 1}).sort([("s", 1), ("n", -1)]).comment("rm397-find")
        ),
    }.items():
        before = _sort_metrics(client)
        for k, v in SORT_BUDGET.items():
            client.admin.command({"setParameter": 1, k: v})
        db.command({"profile": 2})
        try:
            run()
        finally:
            db.command({"profile": 0})
            for k in SORT_BUDGET:
                client.admin.command({"setParameter": 1, k: 104_857_600})
        after = _sort_metrics(client)
        entries = db.system.profile.find(
            {"op": "getmore", "originatingCommand.comment": f"rm397-{name}"}
        )
        out[name] = {
            "getMore": [{k: e.get(k) for k in FIELDS} for e in entries],
            "totalKeysSorted": after["totalKeysSorted"] - before["totalKeysSorted"],
            "totalBytesSorted": after["totalBytesSorted"] - before["totalBytesSorted"],
        }
    return out


@pytest.mark.backlog("RM-400")
def test_get_more_sort_counts(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 profiles a cursor's `getMore` with its sort's counts so
    far (`sortSpills: 52` after an aggregate that reported 52), and adds the
    operation's `keysSorted` / `sortTotalDataSizeBytes` to `totalKeysSorted` /
    `totalBytesSorted` for each operation, the `getMore`s included (2400
    keys for a sort of 300 read through four loads, then one `getMore`)."""
    assert _get_mores(rust_mongo) == _get_mores(mongod)


# ------------------------------------- RM-398 SBE's sort bytes --


def _sbe_sorted(client: Any) -> dict:
    db = _spill_db(client)
    before = _sort_metrics(client)
    list(db.c.aggregate([{"$sort": {"n": 1}}, {"$group": {"_id": "$k", "f": {"$first": "$n"}}}]))
    after = _sort_metrics(client)
    return {k: after[k] - before[k] for k in ("totalKeysSorted", "totalBytesSorted")}


@pytest.mark.backlog_fixed("RM-398")
def test_sbe_sort_bytes(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SBE sort in front of a `$group` counted its keys (300)
    and no bytes in `totalBytesSorted`: it adds bytes only for the runs it
    spills while reading its input."""
    assert _sbe_sorted(rust_mongo) == _sbe_sorted(mongod)


# --------------------------------- RM-399 a sort with a limit spills --


def _top_k(client: Any) -> dict:
    db = _spill_db(client)
    return _counts(
        db,
        {
            "$limit after the sort": [{"$sort": {"n": -1}}, {"$limit": 200}, ADD],
            "$limit moved into the sort": [{"$sort": {"n": -1}}, ADD, {"$limit": 50}],
        },
    )


@pytest.mark.backlog("RM-399")
def test_top_k_sort_spills(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's sort with a limit over its budget spills: 13 runs,
    900 records for a limit of 200 over 300 documents, and 592 records for a
    limit of 50 it moved past an `$addFields`."""
    assert _top_k(rust_mongo) == _top_k(mongod)


# ---------------- found 2026-11-10 while closing RM-398: SBE's sort rows --


def _sbe_sort_spilled(client: Any) -> dict:
    db = _spill_db(client)
    before = _sort_metrics(client)
    entry = _profiled(
        db,
        [{"$sort": {"n": 1}}, {"$group": {"_id": "$k", "f": {"$first": "$s"}}}],
        SORT_BUDGET,
        "rm398-sbe-spilled",
    )
    after = _sort_metrics(client)
    return {
        "totalBytesSorted": after["totalBytesSorted"] - before["totalBytesSorted"],
        "sortTotalDataSizeBytes": entry.get("sortTotalDataSizeBytes"),
    }


@pytest.mark.backlog("RM-504")
def test_sbe_sort_row_leaves_out_a_field_only_the_key_reads(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SBE sort in front of `{$group: {_id: "$k", f:
    {$first: "$s"}}}` keeps `k` and `s` beside its key, not `n`, which only
    the sort reads: a row of 1064, so its six spills while reading count
    274512, where this server's rows of 1073 count 276834."""
    assert _sbe_sort_spilled(rust_mongo) == _sbe_sort_spilled(mongod)


def _sbe_sort_spilled_bytes(client: Any) -> dict:
    db = _spill_db(client)
    entry = _profiled(
        db,
        [{"$sort": {"n": 1}}, {"$group": {"_id": "$k", "f": {"$first": "$s"}}}],
        SORT_BUDGET,
        "rm398-sbe-spilled-bytes",
    )
    return {k: entry.get(k) for k in ("sortSpills", "sortSpilledRecords", "sortSpilledBytes")}


@pytest.mark.backlog("RM-506")
def test_sbe_sort_spilled_bytes(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SBE sort in front of a `$group` writes 13 runs of 900
    records, 927900 bytes (1031 a record); this server writes 928800."""
    assert _sbe_sort_spilled_bytes(rust_mongo) == _sbe_sort_spilled_bytes(mongod)


def _sbe_limited_keys(client: Any) -> dict:
    db = _spill_db(client)
    out = {}
    for limit in (50, 1):
        before = _sort_metrics(client)
        list(
            db.c.aggregate(
                [
                    {"$sort": {"n": 1}},
                    {"$limit": limit},
                    {"$group": {"_id": "$k", "f": {"$first": "$n"}}},
                ]
            )
        )
        after = _sort_metrics(client)
        out[limit] = {k: after[k] - before[k] for k in ("totalKeysSorted", "totalBytesSorted")}
    return out


@pytest.mark.backlog("RM-505")
def test_sbe_limited_sort_counts_every_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SBE sort with a `$limit` in front of a `$group` adds
    every key it took (300 over 300 documents) to `totalKeysSorted`, for a
    limit of 50 and of 1, and no bytes; this server adds 50 and 1."""
    assert _sbe_limited_keys(rust_mongo) == _sbe_limited_keys(mongod)
