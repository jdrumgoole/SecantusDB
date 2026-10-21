"""Gap tests for Rust MongoDB server items found on 2026-10-16 during the
fourth backlog cycle (measured against mongod 8.2.11)."""

from __future__ import annotations

import random
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

CANCELLING = [0.1, 0.2, 0.3, 1e16, 1.0, -1e16]


def _group_sum_avg(client: Any, xs: list) -> tuple:
    pipeline = [
        {"$documents": [{"v": x} for x in xs]},
        {"$group": {"_id": None, "s": {"$sum": "$v"}, "a": {"$avg": "$v"}}},
        {"$project": {"_id": 0}},
    ]
    return mongo_answer(lambda: list(client.admin.aggregate(pipeline)))


@pytest.mark.backlog_fixed("RM-182")
def test_sum_and_avg_accumulators_over_doubles(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sums doubles with its compensated double-double
    summation (`util/summation.h`): `$sum` of 0.1, 0.2, 0.3, 1e16, 1.0,
    -1e16 is 2.0 there, and `$avg` 1/3."""
    assert _group_sum_avg(mongod, CANCELLING) == ("ok", [{"s": 2.0, "a": 1 / 3}])
    assert _group_sum_avg(rust_mongo, CANCELLING) == _group_sum_avg(mongod, CANCELLING)


@pytest.mark.backlog_fixed("RM-182")
def test_sum_accumulator_random_doubles(rust_mongo: Any, mongod: Any) -> None:
    """Random mixes of large and small doubles and integers."""
    rng = random.Random(7)
    sets = []
    for _ in range(30):
        n = rng.randint(2, 12)
        sets.append(
            [
                rng.choice([rng.uniform(-1e17, 1e17), rng.uniform(-1, 1), rng.randint(-1000, 1000)])
                for _ in range(n)
            ]
        )
    assert [_group_sum_avg(rust_mongo, xs) for xs in sets] == [
        _group_sum_avg(mongod, xs) for xs in sets
    ]


def _sum_expression(client: Any) -> tuple:
    pipeline = [
        {"$documents": [{}]},
        {"$project": {"s": {"$sum": [0.1, 0.2, 0.3]}, "a": {"$avg": CANCELLING}}},
    ]
    return mongo_answer(lambda: list(client.admin.aggregate(pipeline)))


@pytest.mark.backlog_fixed("RM-182")
def test_sum_and_avg_expressions_over_doubles(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `{$sum: [0.1, 0.2, 0.3]}` is 0.6 (a plain sum gives
    0.6000000000000001); `$add` is a plain sum there and is not affected."""
    assert _sum_expression(rust_mongo) == _sum_expression(mongod)


WINDOWS = [
    {"documents": [-2, 0]},
    {"documents": [-1, 1]},
    {"documents": [1, 3]},
    {"documents": ["current", 2]},
    {"documents": [-3, -1]},
    {"range": [-5, 5]},
    {"range": [-10, 0]},
]


def _window_answers(client: Any, ops: list[str], edge: bool, eighths: bool = False) -> list:
    """Per window shape and operator, the values written for random
    partitions (`t` repeats, so range windows hold ties). `eighths` draws
    multiples of 1/8 instead of arbitrary doubles (the note above the
    `$stdDev` test says why)."""
    from bson import Decimal128, Int64

    rng = random.Random(23 if edge else 24)
    pool = [
        float("inf"),
        float("-inf"),
        float("nan"),
        Decimal128("NaN"),
        Decimal128("-Infinity"),
        Int64(-(2**63)),
        Int64(2**63 - 1),
        -(2**31),
        2**31 - 1,
        1.5,
        Decimal128("2.25"),
        None,
        "s",
    ]
    out = []
    for _ in range(12):
        n = rng.randint(3, 10)
        docs = [
            {
                "_id": i,
                "t": rng.randint(0, 30),
                "v": rng.choice(pool)
                if edge
                else (rng.randint(-8000, 8000) / 8 if eighths else rng.uniform(-1e3, 1e3)),
            }
            for i in range(n)
        ]
        for w in WINDOWS:
            for op in ops:
                sort = {"t": 1} if "range" in w else {"_id": 1}
                pipeline = [
                    {"$documents": docs},
                    {
                        "$setWindowFields": {
                            "sortBy": sort,
                            "output": {"s": {op: "$v", "window": w}},
                        }
                    },
                    {"$sort": {"_id": 1}},
                ]
                ans = mongo_answer(
                    lambda p=pipeline: [d.get("s") for d in client.admin.aggregate(p)]
                )
                # NaN != NaN: compare a NaN by name.
                out.append(repr(ans))
    return out


# Why the `$stdDev` tests draw multiples of 1/8: mongod computes a window's
# `count * x - sum` with ONE rounding in its macOS arm64 build (clang fuses
# it into an FMA) and two in its Linux x86-64 build, so for arbitrary
# doubles the two builds differ in the last digit (measured 2026-10-21,
# 8.2.11 / 8.2.12 macOS against 8.2.12 Linux in CI). This server follows the
# macOS build (`mul_add`, windowfields.rs). Eighths up to 1,000 make that
# product and sum exact, so every build agrees and the tests still pin the
# incremental arithmetic. Which build to follow is RM-231.


@pytest.mark.backlog_fixed("RM-181")
def test_std_dev_over_range_windows_and_non_finite_values(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 moves a range window by its index interval (adding the
    documents past the old end, then dropping as many as the start moved),
    and its `reset()` forgets the non-finite values still in the window."""
    ops = ["$stdDevPop", "$stdDevSamp"]
    for edge in (False, True):
        ours = _window_answers(rust_mongo, ops, edge, eighths=True)
        assert ours == _window_answers(mongod, ops, edge, eighths=True)


@pytest.mark.backlog_fixed("RM-183")
def test_sum_and_avg_over_removable_windows(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs `$sum` / `$avg` over a window that drops documents
    as a removable sum: a removed value is added negated, NaN and the
    infinities are counted apart, and the answer is narrowed back to the
    types still in the window."""
    ops = ["$sum", "$avg"]
    for edge in (False, True):
        assert _window_answers(rust_mongo, ops, edge) == _window_answers(mongod, ops, edge)
