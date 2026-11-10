"""Gap tests for what working RM-327 (the `$setWindowFields` budgets) found
on 2026-11-08, comparing with mongod 8.2.1 (macOS arm64) and 8.2.12 (Linux
x86-64 and arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

DEFAULT = 104_857_600

# --------------------------------------- RM-405 rank functions' sortBy --

RANK_SPECS = {
    "$documentNumber, no sortBy": {"output": {"t": {"$documentNumber": {}}}},
    "$documentNumber, partitioned": {
        "partitionBy": "$k",
        "output": {"t": {"$documentNumber": {}}},
    },
    "$documentNumber, two sort fields": {
        "sortBy": {"k": 1, "n": 1},
        "output": {"t": {"$documentNumber": {}}},
    },
    "$rank, two sort fields": {"sortBy": {"k": 1, "n": 1}, "output": {"t": {"$rank": {}}}},
    "$denseRank, two sort fields": {
        "sortBy": {"k": 1, "n": -1},
        "output": {"t": {"$denseRank": {}}},
    },
}


def _rank_answers(client: Any) -> dict:
    client.drop_database("rmsortby")
    coll = client.rmsortby.c
    coll.insert_many([{"_id": i, "k": i % 2, "n": i} for i in range(4)])
    return {
        name: mongo_answer(lambda spec=spec: list(coll.aggregate([{"$setWindowFields": spec}])))
        for name, spec in RANK_SPECS.items()
    }


@pytest.mark.backlog_fixed("RM-405")
def test_rank_functions_need_one_sort_field(rust_mongo: Any, mongod: Any) -> None:
    """`$documentNumber` with no `sortBy`, and any rank function over a
    two-field one, was answered; mongod refuses each before reading a
    document (5371602: `... must be specified with a top level sortBy
    expression with exactly one element`)."""
    assert _rank_answers(rust_mongo) == _rank_answers(mongod)


# ------------------------------------ RM-404 more window budgets --

SHAPES = {
    "range [unbounded, 4] $sum": {"$sum": "$n", "window": {"range": ["unbounded", 4]}},
    "$expMovingAvg": {"$expMovingAvg": {"input": "$n", "N": 2}},
    "$locf": {"$locf": "$n"},
    "[-3, 3] $max": {"$max": "$n", "window": {"documents": [-3, 3]}},
}

#: More windows run by the executors RM-404 fixed: a window with no lower
#: end, which lets documents go as its upper end passes them, `$min` /
#: `$max` over values that are and are not on the heap, and the value a
#: `$locf` carries.
MORE_SHAPES = {
    "range [unbounded, 0] $sum": {"$sum": "$n", "window": {"range": ["unbounded", 0]}},
    "range [unbounded, -9] $sum": {"$sum": "$n", "window": {"range": ["unbounded", -9]}},
    "range [unbounded, current] $avg": {
        "$avg": "$n",
        "window": {"range": ["unbounded", "current"]},
    },
    "range [unbounded, 4] $push": {"$push": "$n", "window": {"range": ["unbounded", 4]}},
    "range [-4, current] $first": {"$first": "$n", "window": {"range": [-4, "current"]}},
    "documents [unbounded, -4] $sum": {
        "$sum": "$n",
        "window": {"documents": ["unbounded", -4]},
    },
    "[-3, 3] $min of a string": {"$min": "$s", "window": {"documents": [-3, 3]}},
    "[-3, 3] $max of a missing field": {"$max": "$zz", "window": {"documents": [-3, 3]}},
    "range [-4, 4] $max": {"$max": "$n", "window": {"range": [-4, 4]}},
    "$locf of a string": {"$locf": "$s"},
}


def _shape_budgets(client: Any, shapes: dict = SHAPES, partitioned: bool = False) -> dict:
    client.drop_database("rmshapes")
    coll = client.rmshapes.c
    coll.insert_many([{"_id": i, "k": i % 3, "n": i * 2, "s": "x" * (20 + i)} for i in range(40)])
    out = {}
    for name, output in shapes.items():
        spec: dict = {"sortBy": {"n": 1}, "output": {"t": output}}
        if partitioned:
            spec = {"partitionBy": "$k", **spec}
        pipeline = [{"$setWindowFields": spec}, {"$project": {"_id": 1, "t": 1}}]

        def runs(budget: int, pipeline: list = pipeline) -> bool:
            knob = "internalDocumentSourceSetWindowFieldsMaxMemoryBytes"
            client.admin.command({"setParameter": 1, knob: budget})
            try:
                got = mongo_answer(lambda: list(coll.aggregate(pipeline, allowDiskUse=False)))
            finally:
                client.admin.command({"setParameter": 1, knob: DEFAULT})
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


@pytest.mark.backlog_fixed("RM-404")
def test_window_budget_more_shapes(rust_mongo: Any, mongod: Any) -> None:
    """The least budget Linux mongod 8.2.12 runs these windows under over
    40 small documents: 676 for a `range` window with no lower end (this
    server held the whole partition: 5176), 261 for `$expMovingAvg` and
    253 for `$locf` (this server: 250, nothing for the function), 852 for a
    removable `$max` (this server: 908). macOS mongod's differ where its
    sizes do (RM-403)."""
    assert _shape_budgets(rust_mongo) == _shape_budgets(mongod)


@pytest.mark.backlog_fixed("RM-404")
@pytest.mark.parametrize("partitioned", [False, True], ids=["whole", "partitioned"])
def test_window_budget_unbounded_lower_shapes(
    rust_mongo: Any, mongod: Any, partitioned: bool
) -> None:
    """Measured with RM-404 on macOS mongod 8.2.1: a window with no lower
    end held from the right end it found answering the document before
    (`range`) or from its upper end (`documents`), not the whole partition
    or the document answered; a `range` window ending at `"current"` read
    no further; `$min` / `$max` counted each value twice unless nullish;
    `$locf` counted the buffer of the value it carried."""
    got = _shape_budgets(rust_mongo, MORE_SHAPES, partitioned)
    assert got == _shape_budgets(mongod, MORE_SHAPES, partitioned)
