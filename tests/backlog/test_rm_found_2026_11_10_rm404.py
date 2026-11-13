"""Gap tests for what working RM-404 (more `$setWindowFields` budgets)
found on 2026-11-10, comparing with mongod 8.2.1 (macOS arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.test_rm_found_2026_11_08 import _shape_budgets

# ------------------------------------- RM-508 a range window's "current" --

CURRENT_BOUNDS = [
    ["unbounded", "current"],
    ["current", "unbounded"],
    ["current", "current"],
    [-1, "current"],
]


def _current_windows(client: Any) -> dict:
    client.drop_database("rmcurrent")
    coll = client.rmcurrent.c
    coll.insert_many([{"_id": i, "n": v} for i, v in enumerate([1, 1, 2, 2, 2, 3])])
    out = {}
    for bounds in CURRENT_BOUNDS:
        spec = {
            "sortBy": {"n": 1},
            "output": {"t": {"$push": "$_id", "window": {"range": bounds}}},
        }
        out[str(bounds)] = [d["t"] for d in coll.aggregate([{"$setWindowFields": spec}])]
    return out


@pytest.mark.backlog_fixed("RM-508")
def test_a_range_windows_current_is_the_document(rust_mongo: Any, mongod: Any) -> None:
    """A `range` window's `"current"` bound is the document answered, not
    its sort value: mongod's `["unbounded", "current"]` over `n` of 1, 1, 2,
    2, 2, 3 gives `[0], [0, 1], [0, 1, 2], ...`; this server took in the
    documents tied with it (`[0, 1], [0, 1], [0, 1, 2, 3, 4], ...`)."""
    assert _current_windows(rust_mongo) == _current_windows(mongod)


# --------------------------- RM-509 $integral's and the covariances' memory --

COUNTED_SHAPES = {
    "range [unbounded, 4] $integral": {
        "$integral": {"input": "$n"},
        "window": {"range": ["unbounded", 4]},
    },
    "documents [-3, 3] $integral": {
        "$integral": {"input": "$n"},
        "window": {"documents": [-3, 3]},
    },
    "documents [unbounded, -1] $covariancePop": {
        "$covariancePop": ["$n", "$n"],
        "window": {"documents": ["unbounded", -1]},
    },
    "documents [-3, 3] $covarianceSamp": {
        "$covarianceSamp": ["$n", "$n"],
        "window": {"documents": [-3, 3]},
    },
}


@pytest.mark.backlog_fixed("RM-509")
def test_integral_and_covariance_count_their_memory(rust_mongo: Any, mongod: Any) -> None:
    """mongod counts what `$integral` and the covariances hold (macOS
    8.2.1: `maxFunctionMemoryUsageBytes` 3888 for a `["unbounded", 4]`
    `range` `$integral` over 40 documents, 568 for a `["unbounded", -1]`
    `$covariancePop`); this server counted nothing for them, so their
    least budgets were lower (500 against 4300, 375 against 818)."""
    assert _shape_budgets(rust_mongo, COUNTED_SHAPES) == _shape_budgets(mongod, COUNTED_SHAPES)


# ------------------------- RM-510 a partitioned window's maxTotalMemory --

TOTAL_SHAPES = {
    "range [-4, 4] $sum": {"$sum": "$n", "window": {"range": [-4, 4]}},
    "range [unbounded, unbounded] $sum": {
        "$sum": "$n",
        "window": {"range": ["unbounded", "unbounded"]},
    },
    "documents [unbounded, current] $sum": {
        "$sum": "$n",
        "window": {"documents": ["unbounded", "current"]},
    },
    "$expMovingAvg": {"$expMovingAvg": {"input": "$n", "N": 2}},
    "documents [unbounded, -1] $push": {
        "$push": "$n",
        "window": {"documents": ["unbounded", -1]},
    },
    "documents [unbounded, -1] $integral": {
        "$integral": {"input": "$n"},
        "window": {"documents": ["unbounded", -1]},
    },
}


def _partitioned_totals(client: Any) -> dict:
    client.drop_database("rmtotals")
    coll = client.rmtotals.c
    coll.insert_many([{"_id": i, "k": i % 3, "n": i * 2, "s": "x" * (20 + i)} for i in range(40)])
    out = {}
    for name, output in TOTAL_SHAPES.items():
        spec = {"partitionBy": "$k", "sortBy": {"n": 1}, "output": {"t": output}}
        pipeline = [{"$setWindowFields": spec}, {"$project": {"_id": 1, "t": 1}}]
        explain = client.rmtotals.command(
            "explain",
            {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            verbosity="executionStats",
        )
        stage = next(s for s in explain["stages"] if "$_internalSetWindowFields" in s)
        out[name] = stage["maxTotalMemoryUsageBytes"]
    return out


@pytest.mark.backlog_fixed("RM-510")
def test_a_partitioned_windows_total_memory(rust_mongo: Any, mongod: Any) -> None:
    """A partitioned `$setWindowFields`' `maxTotalMemoryUsageBytes` was 16
    bytes off mongod's (three partitions of 40 small documents): a `range
    [-4, 4]` `$sum` 764 against 748, a whole-partition `$sum` 2196 against
    2180, a running `$sum` 464 against 480, an `$expMovingAvg` 416 against
    432; a `["unbounded", -1]` `$push` 724 against 756 and `$integral` 1836
    against 1940 (RM-509's). mongod's iterator replaces its memory token by assigning a new one
    over the old, so it counts both for a moment: reading the next
    partition's first document, and starting that partition (its first
    document and key twice). This server counted both keys for the whole
    update after the read, and nothing at a partition's start."""
    assert _partitioned_totals(rust_mongo) == _partitioned_totals(mongod)


KEYED_SHAPES = {
    "range [-4, 4] $sum": {"$sum": "$n", "window": {"range": [-4, 4]}},
    "range [unbounded, unbounded] $sum": {
        "$sum": "$n",
        "window": {"range": ["unbounded", "unbounded"]},
    },
    "$first": {"$first": "$n"},
    "documents [unbounded, current] $sum": {
        "$sum": "$n",
        "window": {"documents": ["unbounded", "current"]},
    },
    "documents [-1, 1] $sum": {"$sum": "$n", "window": {"documents": [-1, 1]}},
}


def _keyed_totals(client: Any) -> dict:
    """Three partitions whose keys are 1, 100 and 400 characters, in two
    orders and three shapes, so which key each moment counts shows."""
    out = {}
    for lengths in [(1, 100, 400), (400, 100, 1)]:
        keys = [chr(ord("a") + i) * n for i, n in enumerate(lengths)]
        for sizes in [(10, 10, 10), (20, 5, 5), (5, 5, 20)]:
            client.drop_database("rmkeyed")
            coll = client.rmkeyed.c
            docs, i = [], 0
            for p, count in enumerate(sizes):
                for _ in range(count):
                    docs.append({"_id": i, "k": p, "n": i})
                    i += 1
            coll.insert_many(docs)
            for name, output in KEYED_SHAPES.items():
                spec = {
                    "partitionBy": {"$arrayElemAt": [keys, "$k"]},
                    "sortBy": {"n": 1},
                    "output": {"t": output},
                }
                pipeline = [{"$setWindowFields": spec}, {"$project": {"_id": 1, "t": 1}}]
                explain = client.rmkeyed.command(
                    "explain",
                    {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
                    verbosity="executionStats",
                )
                stage = next(s for s in explain["stages"] if "$_internalSetWindowFields" in s)
                out[(lengths, sizes, name)] = stage["maxTotalMemoryUsageBytes"]
    return out


@pytest.mark.backlog_fixed("RM-510")
def test_which_partition_key_each_moment_counts(rust_mongo: Any, mongod: Any) -> None:
    """With keys of different sizes, mongod 8.2.11 counts this partition's
    key and the next one's while it reads the next partition's first
    document, that partition's key twice as it starts it, and a removable
    function at its own size before its first answer (a five-document
    partition's `range [-4, 4]` `$sum` reads past its end there). This
    server was off by a key in either direction, and by the `$sum`'s 304
    bytes."""
    assert _keyed_totals(rust_mongo) == _keyed_totals(mongod)
