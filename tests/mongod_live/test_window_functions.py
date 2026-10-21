"""`$setWindowFields` and `$fill` against mongod 8.2.

`crates/secantus-core/src/windowfields.rs` and `fill.rs` had no unit tests,
and the only comparisons were the Rust-vs-Python parity suites, retired from
CI on 2026-10-20 (and never evidence of correctness). Each case runs one
pipeline over the same documents on both servers and compares the answers
exactly, types included.
"""

from __future__ import annotations

import datetime
from typing import Any

import pytest
from bson import Decimal128, Int64

from tests.mongod_live.conftest import mongo_answer, same

_T0 = datetime.datetime(2026, 1, 1)


def _day(n: float) -> datetime.datetime:
    return _T0 + datetime.timedelta(days=n)


#: Two partitions, numeric types mixed on purpose (int32, int64, double,
#: decimal), a missing value and a null, and dates for time windows.
DOCS = [
    {"_id": 1, "g": "a", "t": _day(0), "x": 1, "y": 10},
    {"_id": 2, "g": "a", "t": _day(1), "x": Int64(4), "y": 7},
    {"_id": 3, "g": "a", "t": _day(2.5), "x": 2.5, "y": None},
    {"_id": 4, "g": "a", "t": _day(4), "y": 3},
    {"_id": 5, "g": "a", "t": _day(7), "x": 9, "y": 1},
    {"_id": 6, "g": "b", "t": _day(0), "x": Decimal128("1.5"), "y": 2},
    {"_id": 7, "g": "b", "t": _day(3), "x": 6, "y": 4},
    {"_id": 8, "g": "b", "t": _day(6), "x": -2, "y": 8},
]

_DOCS = {"documents": ["unbounded", "current"]}
_SLIDE = {"documents": [-1, 1]}


def _swf(output: dict, *, sort: dict | None = None, partition: bool = True) -> list:
    stage: dict[str, Any] = {"sortBy": sort or {"t": 1}, "output": output}
    if partition:
        stage["partitionBy"] = "$g"
    return [{"$setWindowFields": stage}, {"$sort": {"_id": 1}}]


CASES: dict[str, list] = {
    "sum running": _swf({"r": {"$sum": "$x", "window": _DOCS}}),
    "sum sliding": _swf({"r": {"$sum": "$x", "window": _SLIDE}}),
    "avg sliding": _swf({"r": {"$avg": "$x", "window": _SLIDE}}),
    "min max": _swf(
        {"lo": {"$min": "$x", "window": _DOCS}, "hi": {"$max": "$y", "window": _SLIDE}}
    ),
    "count": _swf({"r": {"$count": {}, "window": _DOCS}}),
    "push and addToSet": _swf(
        {"p": {"$push": "$y", "window": _SLIDE}, "s": {"$addToSet": "$g", "window": _DOCS}}
    ),
    "first last": _swf(
        {"f": {"$first": "$x", "window": _SLIDE}, "l": {"$last": "$x", "window": _SLIDE}}
    ),
    "stdDev": _swf(
        {
            "p": {"$stdDevPop": "$x", "window": _DOCS},
            "s": {"$stdDevSamp": "$x", "window": _DOCS},
        }
    ),
    "rank denseRank documentNumber": _swf(
        {"r": {"$rank": {}}, "d": {"$denseRank": {}}, "n": {"$documentNumber": {}}},
        sort={"y": 1},
    ),
    "shift": _swf(
        {
            "prev": {"$shift": {"output": "$x", "by": -1, "default": "none"}},
            "next": {"$shift": {"output": "$y", "by": 2}},
        }
    ),
    "derivative per day": _swf(
        {"d": {"$derivative": {"input": "$y", "unit": "day"}, "window": {"documents": [-1, 0]}}},
        sort={"t": 1},
    ),
    "integral per hour": _swf(
        {"i": {"$integral": {"input": "$y", "unit": "hour"}, "window": _DOCS}},
        sort={"t": 1},
    ),
    "integral without a unit": _swf(
        {"i": {"$integral": {"input": "$y"}, "window": _DOCS}}, sort={"_id": 1}
    ),
    "covariance": _swf(
        {
            "p": {"$covariancePop": ["$x", "$y"], "window": _DOCS},
            "s": {"$covarianceSamp": ["$x", "$y"], "window": _DOCS},
        }
    ),
    "locf": _swf({"f": {"$locf": "$x"}}),
    "linearFill": _swf({"f": {"$linearFill": "$y"}}, sort={"t": 1}),
    "range window on a date": _swf(
        {"r": {"$sum": "$y", "window": {"range": [-2, 0], "unit": "day"}}}, sort={"t": 1}
    ),
    "range window on a number": _swf(
        {"r": {"$push": "$_id", "window": {"range": [-2, 1]}}}, sort={"_id": 1}
    ),
    "expMovingAvg over numbers": _swf(
        {
            "n": {"$expMovingAvg": {"input": "$_id", "N": 3}},
            "a": {"$expMovingAvg": {"input": "$_id", "alpha": 0.4}},
        },
        sort={"_id": 1},
    ),
    "no partition": _swf({"r": {"$sum": "$y", "window": _DOCS}}, partition=False),
    "minN topN": _swf(
        {
            "m": {"$minN": {"input": "$y", "n": 2}, "window": _DOCS},
            "t": {"$topN": {"output": "$_id", "sortBy": {"y": -1}, "n": 2}, "window": _DOCS},
        }
    ),
    "fill linear": [
        {"$fill": {"sortBy": {"t": 1}, "partitionBy": "$g", "output": {"y": {"method": "linear"}}}},
        {"$sort": {"_id": 1}},
    ],
    "fill locf": [
        {
            "$fill": {
                "sortBy": {"t": 1},
                "partitionByFields": ["g"],
                "output": {"x": {"method": "locf"}},
            }
        },
        {"$sort": {"_id": 1}},
    ],
    "fill value": [
        {"$fill": {"output": {"x": {"value": 0}, "y": {"value": "$_id"}}}},
        {"$sort": {"_id": 1}},
    ],
    # Errors are part of the surface: mongod's code and message.
    # Not here while they diverge, each with a gap test in tests/backlog/:
    # `$expMovingAvg` over null / missing / Decimal128 input (RM-225), and
    # the window-spec errors answered as code 2 (RM-226).
    "derivative without a unit on dates": _swf(
        {"d": {"$derivative": {"input": "$t"}, "window": {"documents": [-1, 0]}}}
    ),
}


@pytest.fixture(scope="module")
def seeded(rust_mongo: Any, mongod: Any) -> tuple[Any, Any]:
    out = []
    for client in (rust_mongo, mongod):
        coll = client["live_windows"]["c"]
        coll.drop()
        coll.insert_many(DOCS)
        out.append(coll)
    return out[0], out[1]


@pytest.mark.parametrize("name", list(CASES))
def test_window_matches_mongod(name: str, seeded: tuple[Any, Any]) -> None:
    ours, theirs = seeded
    pipeline = CASES[name]
    same(
        mongo_answer(lambda: list(ours.aggregate(pipeline))),
        mongo_answer(lambda: list(theirs.aggregate(pipeline))),
    )
