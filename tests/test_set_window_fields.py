"""``$setWindowFields`` — partition + sort + per-row windowed accumulators.

First-cut subset that matches the common driver-test surface:

* The nine ``$group`` accumulators (``$sum`` / ``$avg`` / ``$min`` /
  ``$max`` / ``$first`` / ``$last`` / ``$push`` / ``$addToSet`` /
  ``$count``).
* Position-based windows via ``window: {documents: [<lower>, <upper>]}``.
* Bound forms: integer offsets, ``"current"``, ``"unbounded"``.
* Default window (when not specified) covers the whole partition.

Deferred (raises ``AggregateError`` with a clear message): range-based
windows (``window: {range: [...]}``), time-series functions
(``$derivative`` / ``$integral`` / ``$linearFill`` / ``$locf`` /
``$shift`` / ``$expMovingAvg``), and rank functions (``$rank`` /
``$denseRank`` / ``$documentNumber``).
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield c
    finally:
        c.close()


# ---------------------------------------------------------------------------
# No-partition / default-window cases
# ---------------------------------------------------------------------------


def test_sum_over_whole_partition_no_window(client) -> None:
    """No ``window`` spec → entire partition. With no partitionBy, the
    partition is the whole input. Every row gets the same total."""
    coll = client["swf_db"]["sum_whole"]
    coll.insert_many([{"_id": i, "v": i + 1} for i in range(4)])  # 1+2+3+4 = 10

    pipeline = [
        {
            "$setWindowFields": {
                "output": {"total": {"$sum": "$v"}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    assert [d["total"] for d in docs] == [10, 10, 10, 10]


def test_partition_by_field_splits_partitions(client) -> None:
    """Each partition gets its own total — `category: "a"` rows see one
    total, `category: "b"` rows see another."""
    coll = client["swf_db"]["partition"]
    coll.insert_many(
        [
            {"_id": 1, "category": "a", "v": 10},
            {"_id": 2, "category": "b", "v": 100},
            {"_id": 3, "category": "a", "v": 20},
            {"_id": 4, "category": "b", "v": 200},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "partitionBy": "$category",
                "output": {"cat_total": {"$sum": "$v"}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    assert [d["cat_total"] for d in docs] == [30, 300, 30, 300]


# ---------------------------------------------------------------------------
# Position-based windows
# ---------------------------------------------------------------------------


def test_rolling_window_3_doc_sum(client) -> None:
    """3-doc rolling sum: window is [-1, 1] around the current row.
    Boundary rows clamp to the partition edges."""
    coll = client["swf_db"]["rolling"]
    coll.insert_many([{"_id": i, "v": i + 1} for i in range(5)])  # v: 1,2,3,4,5

    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"_id": 1},
                "output": {
                    "rolling3": {
                        "$sum": "$v",
                        "window": {"documents": [-1, 1]},
                    }
                },
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # Row 0: window=[0,1] → 1+2 = 3
    # Row 1: window=[0,2] → 1+2+3 = 6
    # Row 2: window=[1,3] → 2+3+4 = 9
    # Row 3: window=[2,4] → 3+4+5 = 12
    # Row 4: window=[3,4] → 4+5 = 9
    assert [d["rolling3"] for d in docs] == [3, 6, 9, 12, 9]


def test_unbounded_lower_to_current_running_total(client) -> None:
    """``[unbounded, current]`` is the canonical running-total window."""
    coll = client["swf_db"]["running"]
    coll.insert_many([{"_id": i, "v": i + 1} for i in range(4)])

    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"_id": 1},
                "output": {
                    "running": {
                        "$sum": "$v",
                        "window": {"documents": ["unbounded", "current"]},
                    }
                },
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # 1, 1+2=3, 1+2+3=6, 1+2+3+4=10
    assert [d["running"] for d in docs] == [1, 3, 6, 10]


def test_unbounded_both_sides_per_partition_total(client) -> None:
    """``[unbounded, unbounded]`` == default window. Every row gets the
    partition total."""
    coll = client["swf_db"]["unbounded"]
    coll.insert_many(
        [
            {"_id": 1, "cat": "a", "v": 10},
            {"_id": 2, "cat": "a", "v": 20},
            {"_id": 3, "cat": "b", "v": 100},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "partitionBy": "$cat",
                "sortBy": {"_id": 1},
                "output": {
                    "cat_sum": {
                        "$sum": "$v",
                        "window": {"documents": ["unbounded", "unbounded"]},
                    }
                },
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    assert [d["cat_sum"] for d in docs] == [30, 30, 100]


def test_avg_first_last_min_max_over_window(client) -> None:
    """All five basic accumulators in one stage, [-1, 1] window."""
    coll = client["swf_db"]["multi"]
    coll.insert_many([{"_id": i, "v": (i + 1) * 10} for i in range(4)])

    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"_id": 1},
                "output": {
                    "avg3": {"$avg": "$v", "window": {"documents": [-1, 1]}},
                    "min3": {"$min": "$v", "window": {"documents": [-1, 1]}},
                    "max3": {"$max": "$v", "window": {"documents": [-1, 1]}},
                    "first3": {"$first": "$v", "window": {"documents": [-1, 1]}},
                    "last3": {"$last": "$v", "window": {"documents": [-1, 1]}},
                },
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # Row 0: window [0,1] = [10,20] → avg=15, min=10, max=20, first=10, last=20
    # Row 1: window [0,2] = [10,20,30] → avg=20, min=10, max=30, first=10, last=30
    # Row 2: window [1,3] = [20,30,40] → avg=30, min=20, max=40, first=20, last=40
    # Row 3: window [2,3] = [30,40] → avg=35, min=30, max=40, first=30, last=40
    assert [d["avg3"] for d in docs] == [15.0, 20.0, 30.0, 35.0]
    assert [d["min3"] for d in docs] == [10, 10, 20, 30]
    assert [d["max3"] for d in docs] == [20, 30, 40, 40]
    assert [d["first3"] for d in docs] == [10, 10, 20, 30]
    assert [d["last3"] for d in docs] == [20, 30, 40, 40]


def test_count_over_window(client) -> None:
    """``$count`` ignores the arg and counts docs in the window."""
    coll = client["swf_db"]["count"]
    coll.insert_many([{"_id": i} for i in range(5)])

    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"_id": 1},
                "output": {
                    "around": {"$count": {}, "window": {"documents": [-1, 1]}},
                },
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    assert [d["around"] for d in docs] == [2, 3, 3, 3, 2]


def test_push_and_add_to_set_over_window(client) -> None:
    """``$push`` collects ordered, ``$addToSet`` collects unique."""
    coll = client["swf_db"]["collect"]
    coll.insert_many(
        [
            {"_id": 1, "tag": "x"},
            {"_id": 2, "tag": "y"},
            {"_id": 3, "tag": "x"},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"_id": 1},
                "output": {
                    "tags": {
                        "$push": "$tag",
                        "window": {"documents": ["unbounded", "current"]},
                    },
                    "uniq": {
                        "$addToSet": "$tag",
                        "window": {"documents": ["unbounded", "current"]},
                    },
                },
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    assert docs[0]["tags"] == ["x"] and docs[0]["uniq"] == ["x"]
    assert docs[1]["tags"] == ["x", "y"] and sorted(docs[1]["uniq"]) == ["x", "y"]
    assert docs[2]["tags"] == ["x", "y", "x"] and sorted(docs[2]["uniq"]) == ["x", "y"]


# ---------------------------------------------------------------------------
# Sort within partition
# ---------------------------------------------------------------------------


def test_partition_sort_changes_running_total_order(client) -> None:
    """Within each partition, sortBy controls the running-total order
    independently of input order. The original doc order is preserved
    in the output; only the computed field reflects the sorted order."""
    coll = client["swf_db"]["psort"]
    coll.insert_many(
        [
            {"_id": 1, "cat": "a", "ts": 30, "v": 3},
            {"_id": 2, "cat": "a", "ts": 10, "v": 1},
            {"_id": 3, "cat": "a", "ts": 20, "v": 2},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "partitionBy": "$cat",
                "sortBy": {"ts": 1},
                "output": {
                    "running": {
                        "$sum": "$v",
                        "window": {"documents": ["unbounded", "current"]},
                    }
                },
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # In ts-order: ts=10 (v=1, running=1), ts=20 (v=2, running=3), ts=30 (v=3, running=6)
    # _id=1 has ts=30 → running=6, _id=2 has ts=10 → running=1, _id=3 has ts=20 → running=3
    assert [(d["_id"], d["running"]) for d in docs] == [(1, 6), (2, 1), (3, 3)]


# ---------------------------------------------------------------------------
# Edge cases / errors
# ---------------------------------------------------------------------------


def test_unsupported_time_series_function_raises(client) -> None:
    """Time-series functions are still deferred — raise so the gap is
    visible. (Rank functions shipped separately — see
    ``tests/test_window_rank_functions.py`` for their semantics.)"""
    coll = client["swf_db"]["timeseries"]
    coll.insert_one({"_id": 1, "v": 1, "ts": 1})

    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$setWindowFields": {
                            "sortBy": {"ts": 1},
                            "output": {"d": {"$derivative": {"input": "$v"}}},
                        }
                    }
                ]
            )
        )


def test_range_window_not_yet_implemented(client) -> None:
    """Range-based windows raise rather than silently doing the wrong thing."""
    coll = client["swf_db"]["range"]
    coll.insert_one({"_id": 1, "ts": 1, "v": 1})

    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$setWindowFields": {
                            "sortBy": {"ts": 1},
                            "output": {
                                "x": {
                                    "$sum": "$v",
                                    "window": {"range": [-1, 1]},
                                }
                            },
                        }
                    }
                ]
            )
        )


def test_missing_output_rejected(client) -> None:
    coll = client["swf_db"]["bad"]
    coll.insert_one({"_id": 1})

    with pytest.raises(OperationFailure):
        list(coll.aggregate([{"$setWindowFields": {}}]))


def test_multiple_accumulators_in_one_output_rejected(client) -> None:
    """Each output field must have exactly one accumulator."""
    coll = client["swf_db"]["bad2"]
    coll.insert_one({"_id": 1, "v": 1})

    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [{"$setWindowFields": {"output": {"bad": {"$sum": "$v", "$avg": "$v"}}}}]
            )
        )


def test_empty_input_returns_empty(client) -> None:
    """Empty collection → empty result, no errors."""
    coll = client["swf_db"]["empty"]
    pipeline = [
        {"$setWindowFields": {"output": {"total": {"$sum": "$v"}}}},
    ]
    assert list(coll.aggregate(pipeline)) == []


def test_preserves_original_input_order(client) -> None:
    """The output is in input order, NOT sort-by order. Internally we
    partition + sort to compute the new fields, but emit rows in the
    same order they came in."""
    coll = client["swf_db"]["order"]
    # Insert in non-monotonic ts order. Without a downstream sort, the
    # pipeline preserves the storage iteration order (which for an
    # int _id is _id-asc, matching the natural order docs came in).
    coll.insert_many(
        [
            {"_id": 1, "ts": 30},
            {"_id": 2, "ts": 10},
            {"_id": 3, "ts": 20},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"ts": 1},
                "output": {
                    "running": {
                        "$sum": 1,
                        "window": {"documents": ["unbounded", "current"]},
                    }
                },
            }
        },
    ]
    docs = list(coll.aggregate(pipeline))
    # _id order preserved in output.
    assert [d["_id"] for d in docs] == [1, 2, 3]
    # In ts-asc order: ts=10 (_id=2, running=1), ts=20 (_id=3, running=2),
    # ts=30 (_id=1, running=3). So _id=1 → 3, _id=2 → 1, _id=3 → 2.
    assert [d["running"] for d in docs] == [3, 1, 2]
