"""A pipeline's own `$_internalSetWindowFields` ranks by the sort key the
documents carry, against mongod 8.2 (RM-410).

mongod's `$rank` and `$denseRank` compare the sort-key metadata a `$sort` in
front attached, not the stage's `sortBy`: with no `$sort` in reach every
document ties, a `$sort` on another field ranks by that field, and an empty
array's key (`undefined`) is not a null's. The same key is what the
partition cache and a rank function count in `explain`.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import mongo_answer, same

DOCS = [
    {"_id": i, "n": n, "k": k, "a": a}
    for i, (n, k, a) in enumerate(
        [(3, 1, [2, 9]), (1, 0, [5]), (2, 1, [1, 3]), (2, 0, 7), (5, 1, []), (4, 1, None)]
    )
]

RANKS = {"r": {"$rank": {}}, "d": {"$denseRank": {}}}


def _window(output: dict, **extra: Any) -> dict:
    return {"$_internalSetWindowFields": {"sortBy": {"n": 1}, "output": output, **extra}}


CASES: dict[str, list] = {
    "no sort": [_window(RANKS)],
    "no sort, partitioned": [_window(RANKS, partitionBy="$k")],
    "behind its own sortBy": [{"$sort": {"n": 1}}, _window(RANKS)],
    "behind a sort on another field": [{"$sort": {"k": 1}}, _window(RANKS)],
    "behind a two-field sort": [{"$sort": {"k": 1, "n": -1}}, _window(RANKS)],
    "behind a sort on arrays": [{"$sort": {"a": 1}}, _window(RANKS)],
    "behind a descending sort on arrays": [{"$sort": {"a": -1}}, _window(RANKS)],
    "through stages that keep the key": [
        {"$sort": {"k": 1}},
        {"$match": {"n": {"$gte": 0}}},
        {"$project": {"n": 1, "k": 1}},
        {"$addFields": {"z": 1}},
        {"$skip": 0},
        {"$limit": 10},
        _window(RANKS),
    ],
    "through an unwind": [{"$sort": {"k": 1}}, {"$unwind": "$a"}, _window(RANKS)],
    "a second window": [
        {"$sort": {"k": 1}},
        _window({"x": {"$documentNumber": {}}}),
        _window({"r2": {"$rank": {}}}),
    ],
    "a $meta read beside it": [
        {"$sort": {"k": 1}},
        {"$addFields": {"m": {"$meta": "sortKey"}}},
        _window(RANKS),
        {"$addFields": {"m2": {"$meta": "sortKey"}}},
    ],
    "a limit after it": [{"$sort": {"k": 1}}, {"$limit": 4}, _window(RANKS), {"$limit": 2}],
}


def _load(client: Any) -> Any:
    client.drop_database("internal_window_sort_key")
    db = client.internal_window_sort_key
    db.c.insert_many(DOCS)
    return db


@pytest.mark.parametrize("name", list(CASES))
def test_ranks_follow_the_sort_key(rust_mongo: Any, mongod: Any, name: str) -> None:
    pipeline = CASES[name]
    same(
        mongo_answer(lambda: list(_load(rust_mongo).c.aggregate(pipeline))),
        mongo_answer(lambda: list(_load(mongod).c.aggregate(pipeline))),
    )


def _key_memory(client: Any) -> dict:
    """What each shape's counters hold beyond the window behind its own
    `sortBy`: the key alone, since a function's own size differs between
    mongod's builds (RM-397, RM-403)."""

    def counters(pipeline: list) -> dict:
        ex = _load(client).command(
            {
                "explain": {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
                "verbosity": "executionStats",
            }
        )
        stage = ex["stages"][-1]
        functions = stage["maxFunctionMemoryUsageBytes"]
        return {"total": stage["maxTotalMemoryUsageBytes"], **functions}

    base = counters(CASES["behind its own sortBy"])
    shapes = ["no sort", "behind a sort on another field", "behind a two-field sort"]
    return {name: {k: v - base[k] for k, v in counters(CASES[name]).items()} for name in shapes}


def test_explain_counts_the_key_the_ranks_hold(rust_mongo: Any, mongod: Any) -> None:
    """With no `$sort` the key held is the empty array, 40 bytes more than
    an int's in each rank function (mongod 8.2.11, 2026-11-10); a two-field
    key is an array."""
    same(_key_memory(rust_mongo), _key_memory(mongod))
