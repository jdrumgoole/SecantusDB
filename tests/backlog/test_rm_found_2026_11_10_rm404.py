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


@pytest.mark.backlog("RM-510")
def test_a_partitioned_windows_total_memory(rust_mongo: Any, mongod: Any) -> None:
    """A partitioned `$setWindowFields`' `maxTotalMemoryUsageBytes` is 16
    bytes off mongod's (macOS 8.2.1, three partitions of 40 small
    documents): a `range [-4, 4]` `$sum` 764 against 748, a whole-partition
    `$sum` 2196 against 2180, a running `$sum` 464 against 480, an
    `$expMovingAvg` 416 against 432, in both directions; one partition key
    is 16 bytes, so the iterator's key or next-document accounting is the
    suspect (cause not yet traced). The least budgets agree."""
    assert _partitioned_totals(rust_mongo) == _partitioned_totals(mongod)
