"""``$setWindowFields`` — partition + sort + per-row windowed accumulators.

First-cut subset that matches the common driver-test surface:

* The nine ``$group`` accumulators (``$sum`` / ``$avg`` / ``$min`` /
  ``$max`` / ``$first`` / ``$last`` / ``$push`` / ``$addToSet`` /
  ``$count``).
* Position-based windows via ``window: {documents: [<lower>, <upper>]}``.
* Value-based windows via ``window: {range: [<lower>, <upper>]}`` over a single
  ascending numeric sortBy field (bounds ``[cur+lo, cur+hi]``), or over a date
  sortBy with a fixed-duration ``unit`` (``week`` / ``day`` / ``hour`` /
  ``minute`` / ``second`` / ``millisecond``) that scales the offsets.
* Bound forms: integer offsets, ``"current"``, ``"unbounded"``.
* Default window (when not specified) covers the whole partition.

Also supported: the time-series operators ``$shift`` / ``$expMovingAvg`` /
``$locf`` / ``$linearFill`` / ``$derivative`` / ``$integral``, and the rank
functions (``$rank`` / ``$denseRank`` / ``$documentNumber`` — see
``tests/test_window_rank_functions.py``).

Deferred (raises ``AggregateError`` with a clear message): range windows with a
variable-length ``unit`` (``month`` / ``quarter`` / ``year``) or a non-ascending
/ multi-field / non-numeric sortBy, and ``$derivative`` / ``$integral`` with a
time ``unit``.
"""

from __future__ import annotations

import datetime as _dt

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


def test_shift_prev_next_with_default(client) -> None:
    """`$shift` reads the `output` expression from the row `by` positions away in
    the sorted partition, falling to `default` / null past the edge."""
    coll = client["swf_db"]["shift"]
    coll.insert_many([{"_id": i, "t": i, "v": (i + 1) * 10} for i in range(4)])  # v: 10,20,30,40
    out = list(
        coll.aggregate(
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "prev": {"$shift": {"output": "$v", "by": -1, "default": 0}},
                            "next": {"$shift": {"output": "$v", "by": 1}},
                        },
                    }
                },
                {"$sort": {"_id": 1}},
            ]
        )
    )
    assert [d["prev"] for d in out] == [0, 10, 20, 30]
    assert [d["next"] for d in out] == [20, 30, 40, None]


def test_shift_partitioned(client) -> None:
    """`$shift` is per-partition — it never reads across a partition boundary."""
    coll = client["swf_db"]["shift_part"]
    coll.insert_many(
        [
            {"_id": 1, "g": "a", "t": 1, "v": 1},
            {"_id": 2, "g": "a", "t": 2, "v": 2},
            {"_id": 3, "g": "b", "t": 1, "v": 9},
        ]
    )
    out = list(
        coll.aggregate(
            [
                {
                    "$setWindowFields": {
                        "partitionBy": "$g",
                        "sortBy": {"t": 1},
                        "output": {"nxt": {"$shift": {"output": "$v", "by": 1, "default": -1}}},
                    }
                },
                {"$sort": {"_id": 1}},
            ]
        )
    )
    # a: [1->2, 2->default], b: single row -> default.
    assert [d["nxt"] for d in out] == [2, -1, -1]


def test_exp_moving_avg_n_and_alpha(client) -> None:
    """`$expMovingAvg` — `ema[i] = v[i]*a + ema[i-1]*(1-a)`, with `a = 2/(N+1)`
    for the N form (N=3 -> a=0.5) or an explicit `alpha`."""
    coll = client["swf_db"]["ema"]
    coll.insert_many([{"_id": i, "t": i, "v": v} for i, v in enumerate([10, 20, 30, 40])])
    out = list(
        coll.aggregate(
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "eN": {"$expMovingAvg": {"input": "$v", "N": 3}},
                            "eA": {"$expMovingAvg": {"input": "$v", "alpha": 0.5}},
                        },
                    }
                },
                {"$sort": {"_id": 1}},
            ]
        )
    )
    assert [d["eN"] for d in out] == [10.0, 15.0, 22.5, 31.25]
    assert [d["eA"] for d in out] == [10.0, 15.0, 22.5, 31.25]


def test_exp_moving_avg_requires_one_of_n_alpha(client) -> None:
    """Exactly one of N / alpha — neither or both raises."""
    coll = client["swf_db"]["ema_bad"]
    coll.insert_one({"_id": 1, "t": 1, "v": 1})
    for spec in ({"input": "$v"}, {"input": "$v", "N": 3, "alpha": 0.5}):
        with pytest.raises(OperationFailure):
            list(
                coll.aggregate(
                    [
                        {
                            "$setWindowFields": {
                                "sortBy": {"t": 1},
                                "output": {"e": {"$expMovingAvg": spec}},
                            }
                        }
                    ]
                )
            )


def test_locf_and_linear_fill(client) -> None:
    """`$locf` carries the last non-null forward; `$linearFill` interpolates on
    the sortBy x-axis. Leading nulls (locf) / trailing nulls (both) stay null."""
    coll = client["swf_db"]["fill"]
    coll.insert_many(
        [
            {"_id": 1, "t": 0, "v": None},
            {"_id": 2, "t": 1, "v": 10},
            {"_id": 3, "t": 2, "v": None},
            {"_id": 4, "t": 4, "v": 40},
            {"_id": 5, "t": 5, "v": None},
        ]
    )
    out = list(
        coll.aggregate(
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {"lo": {"$locf": "$v"}, "li": {"$linearFill": "$v"}},
                    }
                },
                {"$sort": {"_id": 1}},
            ]
        )
    )
    assert [d["lo"] for d in out] == [None, 10, 10, 40, 40]
    # t=1..4 line from (1,10) to (4,40): at t=2 -> 20, at t=3 -> 30 (not present),
    # but here t goes 0,1,2,4,5: interp at t=2 -> 10+(40-10)*(2-1)/(4-1) = 20.
    assert [d["li"] for d in out] == [None, 10, 20.0, 40, None]


def test_derivative_unit_on_numeric_sort_raises(client) -> None:
    """A `$derivative` / `$integral` `unit` requires a date sortBy — a numeric
    sortBy raises."""
    coll = client["swf_db"]["timeseries"]
    coll.insert_one({"_id": 1, "v": 1, "ts": 1})

    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$setWindowFields": {
                            "sortBy": {"ts": 1},
                            "output": {"d": {"$derivative": {"input": "$v", "unit": "second"}}},
                        }
                    }
                ]
            )
        )


def test_derivative_and_integral_with_time_unit(client) -> None:
    """`$derivative` / `$integral` with a time `unit` over a date sortBy: the
    x-axis is the date scaled into the unit, so the rate is *per hour*."""
    coll = client["swf_db"]["ts_unit"]
    coll.insert_many(
        [
            {"_id": i, "t": _dt.datetime(2020, 1, 1, i, tzinfo=_dt.timezone.utc), "v": v}
            for i, v in enumerate([0, 10, 30])
        ]
    )
    out = list(
        coll.aggregate(
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            # slope over the whole partition: (30-0)/2h = 15/hour
                            "d": {"$derivative": {"input": "$v", "unit": "hour"}},
                            # trapezoidal area: 5 + 20 = 25 (x in hours)
                            "i": {"$integral": {"input": "$v", "unit": "hour"}},
                        },
                    }
                },
                {"$sort": {"_id": 1}},
            ]
        )
    )
    assert [d["d"] for d in out] == [15.0, 15.0, 15.0]
    assert [d["i"] for d in out] == [25.0, 25.0, 25.0]


def test_derivative_variable_length_unit_raises(client) -> None:
    """A variable-length `unit` (month/quarter/year) on `$derivative` defers."""
    coll = client["swf_db"]["ts_month"]
    coll.insert_many(
        [
            {"_id": i, "t": _dt.datetime(2020, 1 + i, 1, tzinfo=_dt.timezone.utc), "v": i}
            for i in range(3)
        ]
    )
    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$setWindowFields": {
                            "sortBy": {"t": 1},
                            "output": {"d": {"$derivative": {"input": "$v", "unit": "month"}}},
                        }
                    }
                ]
            )
        )


def test_derivative_and_integral(client) -> None:
    """`$derivative` is the slope over the window (null for <2 points);
    `$integral` is the trapezoidal area, both against the sortBy x-axis."""
    coll = client["swf_db"]["deriv"]
    coll.insert_many(
        [{"_id": i, "t": t, "v": v} for i, (t, v) in enumerate([(0, 0), (1, 10), (2, 20), (4, 60)])]
    )
    out = list(
        coll.aggregate(
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "d": {"$derivative": {"input": "$v"}},  # (60-0)/(4-0) = 15
                            "i": {"$integral": {"input": "$v"}},  # 5 + 15 + 80 = 100
                            "rd": {
                                "$derivative": {"input": "$v"},
                                "window": {"documents": [-1, 0]},
                            },
                        },
                    }
                },
                {"$sort": {"_id": 1}},
            ]
        )
    )
    assert [d["d"] for d in out] == [15.0, 15.0, 15.0, 15.0]
    assert [d["i"] for d in out] == [100.0, 100.0, 100.0, 100.0]
    # rolling 2-doc slope: first row has a single-point window -> null.
    assert [d["rd"] for d in out] == [None, 10.0, 10.0, 20.0]


def test_range_window_rolling_sum(client) -> None:
    """Value-based window: include rows whose sortBy value is within
    ``[cur - 1, cur]``. A gap in the sort values (no t=4) shrinks the window."""
    coll = client["swf_db"]["range_roll"]
    coll.insert_many(
        [
            {"_id": 1, "t": 1, "v": 10},
            {"_id": 2, "t": 2, "v": 20},
            {"_id": 3, "t": 3, "v": 30},
            {"_id": 4, "t": 5, "v": 50},
        ]
    )
    out = list(
        coll.aggregate(
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {"s": {"$sum": "$v", "window": {"range": [-1, 0]}}},
                    }
                },
                {"$sort": {"_id": 1}},
            ]
        )
    )
    # t=1 -> {t in [0,1]} = 10; t=2 -> {1,2} = 30; t=3 -> {2,3} = 50;
    # t=5 -> {t in [4,5]} = 50 (t=3 is outside, no t=4).
    assert [d["s"] for d in out] == [10, 30, 50, 50]


def test_range_window_unbounded_to_current_running_total(client) -> None:
    coll = client["swf_db"]["range_run"]
    coll.insert_many([{"_id": i, "t": i, "v": (i + 1) * 10} for i in range(4)])
    out = list(
        coll.aggregate(
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "r": {"$sum": "$v", "window": {"range": ["unbounded", "current"]}}
                        },
                    }
                },
                {"$sort": {"_id": 1}},
            ]
        )
    )
    assert [d["r"] for d in out] == [10, 30, 60, 100]


def test_range_window_time_unit_on_numeric_sort_raises(client) -> None:
    """A range window ``unit`` requires a date sortBy — a numeric sortBy raises."""
    coll = client["swf_db"]["range_unit"]
    coll.insert_one({"_id": 1, "t": 1, "v": 1})
    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$setWindowFields": {
                            "sortBy": {"t": 1},
                            "output": {
                                "x": {"$sum": "$v", "window": {"range": [-1, 0], "unit": "day"}}
                            },
                        }
                    }
                ]
            )
        )


def test_range_window_date_unit_day_rolling_sum(client) -> None:
    """A ``unit: "day"`` range window over a date sortBy sums the trailing
    2-day span for each row (x-axis is the date's epoch millis)."""
    coll = client["swf_db"]["range_day"]
    coll.insert_many(
        [
            {
                "_id": i,
                "t": _dt.datetime(2020, 1, 1 + i, tzinfo=_dt.timezone.utc),
                "v": (i + 1) * 10,
            }
            for i in range(5)
        ]
    )
    out = list(
        coll.aggregate(
            [
                {
                    "$setWindowFields": {
                        "sortBy": {"t": 1},
                        "output": {
                            "s": {"$sum": "$v", "window": {"range": [-2, 0], "unit": "day"}}
                        },
                    }
                },
                {"$sort": {"_id": 1}},
            ]
        )
    )
    assert [d["s"] for d in out] == [10, 30, 60, 90, 120]


def test_range_window_date_without_unit_raises(client) -> None:
    """A range window over a date sortBy requires a ``unit``."""
    coll = client["swf_db"]["range_nounit"]
    coll.insert_one({"_id": 1, "t": _dt.datetime(2020, 1, 1, tzinfo=_dt.timezone.utc), "v": 1})
    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$setWindowFields": {
                            "sortBy": {"t": 1},
                            "output": {"x": {"$sum": "$v", "window": {"range": [-1, 0]}}},
                        }
                    }
                ]
            )
        )


def test_range_window_variable_length_unit_raises(client) -> None:
    """A variable-length ``unit`` (month/quarter/year) is still deferred."""
    coll = client["swf_db"]["range_month"]
    coll.insert_one({"_id": 1, "t": _dt.datetime(2020, 1, 1, tzinfo=_dt.timezone.utc), "v": 1})
    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$setWindowFields": {
                            "sortBy": {"t": 1},
                            "output": {
                                "x": {"$sum": "$v", "window": {"range": [-1, 0], "unit": "month"}}
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
