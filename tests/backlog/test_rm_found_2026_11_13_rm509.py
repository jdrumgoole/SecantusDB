"""Gap tests for what working RM-509 (`$integral`'s and the covariances'
memory) found on 2026-11-13, comparing with mongod 8.2.11 (macOS arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _window_stats(client: Any, docs: list, output: dict) -> tuple:
    """`maxFunctionMemoryUsageBytes` and `maxTotalMemoryUsageBytes` of a
    `$setWindowFields` sorted by `n` over `docs`."""
    client.drop_database("rm509")
    client.rm509.c.insert_many(docs)
    spec = {"sortBy": {"n": 1}, "output": {"t": output}}
    pipeline = [{"$setWindowFields": spec}, {"$project": {"_id": 1, "t": 1}}]
    explain = client.rm509.command(
        "explain",
        {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
        verbosity="executionStats",
    )
    stage = next(s for s in explain["stages"] if "$_internalSetWindowFields" in s)
    return stage["maxFunctionMemoryUsageBytes"]["t"], stage["maxTotalMemoryUsageBytes"]


# ------------------------- RM-617 explain of a failing $setWindowFields --

FAILING_WINDOWS = {
    "$sum of a division by zero": {
        "$sum": {"$divide": [1, "$z"]},
        "window": {"documents": [-1, 1]},
    },
    "$integral of null": {"$integral": {"input": "$z"}, "window": {"documents": [-1, 1]}},
    "$derivative of null": {"$derivative": {"input": "$z"}, "window": {"documents": [-1, 1]}},
}


def _explain_codes(client: Any) -> dict:
    client.drop_database("rm617")
    coll = client.rm617.c
    coll.insert_many(
        [{"_id": i, "z": 0 if i == 2 else (None if i == 1 else i), "s": i} for i in range(5)]
    )
    out = {}
    for name, output in FAILING_WINDOWS.items():
        pipeline = [{"$setWindowFields": {"sortBy": {"s": 1}, "output": {"t": output}}}]
        got = mongo_answer(
            lambda pipeline=pipeline: client.rm617.command(
                "explain",
                {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
                verbosity="executionStats",
            )
        )
        out[name] = got[:2] if got[0] == "err" else ("ok",)
    return out


@pytest.mark.backlog("RM-617")
def test_explain_reports_a_window_functions_error(rust_mongo: Any, mongod: Any) -> None:
    """mongod runs the pipeline for `executionStats` and returns the
    executor's error (8.2.11: codes 2, 5423900 and 5624903 for these three
    windows); this server returns an `explain` document for each."""
    assert _explain_codes(rust_mongo) == _explain_codes(mongod)


# ---------------------------------------- RM-618 $derivative's cache --

DERIVATIVE_WINDOWS = {
    "[-3, 3]": [-3, 3],
    "[-1, 1]": [-1, 1],
    "[0, 0]": [0, 0],
    "[-5, 0]": [-5, 0],
    "[unbounded, current]": ["unbounded", "current"],
    "[unbounded, -1]": ["unbounded", -1],
}


def _derivative_totals(client: Any) -> dict:
    docs = [{"_id": i, "n": i * 2} for i in range(10)]
    return {
        name: _window_stats(
            client, docs, {"$derivative": {"input": "$n"}, "window": {"documents": w}}
        )
        for name, w in DERIVATIVE_WINDOWS.items()
    }


@pytest.mark.backlog("RM-618")
def test_a_derivatives_cache_holds_what_mongods_does(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `maxTotalMemoryUsageBytes` for a `$derivative` over
    ten documents of 125 bytes: 1250 with no lower end, 1000 for `[-3, 3]`
    and `[-5, 0]`, 500 for `[-1, 1]`, 375 for `[0, 0]` (the function holds
    nothing on either server); this server's cache keeps 250 or 500."""
    assert _derivative_totals(rust_mongo) == _derivative_totals(mongod)


# ------------------------------ RM-619 a window that never holds a value --

EMPTY_WINDOWS = [
    (1, "$sum", ["unbounded", -1]),
    (1, "$push", ["unbounded", -1]),
    (1, "$avg", ["unbounded", -1]),
    (2, "$sum", ["unbounded", -2]),
    (2, "$push", ["unbounded", -2]),
    (2, "$avg", ["unbounded", -2]),
]


def _empty_window_functions(client: Any) -> dict:
    out = {}
    for n, op, bounds in EMPTY_WINDOWS:
        docs = [{"_id": i, "n": i * 2} for i in range(n)]
        output = {op: "$n", "window": {"documents": bounds}}
        out[f"{n} {op} {bounds}"] = _window_stats(client, docs, output)[0]
    return out


@pytest.mark.backlog("RM-619")
def test_an_empty_window_counts_no_function(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counts a function with no lower end as 0 while its
    window has held no document (`maxFunctionMemoryUsageBytes`); this
    server counts the accumulator's size, and `$push` the first value."""
    assert _empty_window_functions(rust_mongo) == _empty_window_functions(mongod)
