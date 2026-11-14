"""Gap test for what working RM-402 found, 2026-11-10: `find`, a leading
`$sort` and `$setWindowFields` handed back documents whose sort keys tie in
the order they were read, where mongod's sorter reorders them.
"""

from __future__ import annotations

from typing import Any

import pytest


def _tied_order(client: Any) -> dict:
    client.drop_database("rmtie")
    c = client.rmtie.c
    c.insert_many([{"_id": i, "k": i % 3} for i in range(40)])
    window = {"sortBy": {"k": 1}, "output": {"r": {"$documentNumber": {}}}}
    return {
        "find": [d["_id"] for d in c.find({}).sort("k", 1)],
        "leading $sort": [d["_id"] for d in c.aggregate([{"$sort": {"k": 1}}])],
        "$setWindowFields": [d["_id"] for d in c.aggregate([{"$setWindowFields": window}])],
    }


def _window_tied_orders(client: Any) -> dict:
    client.drop_database("rmtiewin")
    c = client.rmtiewin.c
    c.insert_many([{"_id": i, "k": i % 3, "p": i % 2, "s": "x" * 300} for i in range(60)])
    num = {"r": {"$documentNumber": {}}}
    pipelines = {
        "partition only": [
            {"$setWindowFields": {"partitionBy": "$p", "output": {"t": {"$sum": 1}}}}
        ],
        "partition and sortBy": [
            {"$setWindowFields": {"partitionBy": "$p", "sortBy": {"k": 1}, "output": num}}
        ],
        "descending": [{"$setWindowFields": {"sortBy": {"k": -1}, "output": num}}],
        "after $match": [
            {"$match": {"_id": {"$gte": 5}}},
            {"$setWindowFields": {"sortBy": {"k": 1}, "output": num}},
        ],
        "computed partition": [
            {
                "$setWindowFields": {
                    "partitionBy": {"$mod": ["$_id", 2]},
                    "sortBy": {"k": 1},
                    "output": num,
                }
            }
        ],
        "after $addFields": [
            {"$addFields": {"z": 1}},
            {"$setWindowFields": {"sortBy": {"k": 1}, "output": num}},
        ],
        "$limit after": [
            {"$setWindowFields": {"sortBy": {"k": 1}, "output": num}},
            {"$limit": 7},
        ],
    }
    out = {name: [d["_id"] for d in c.aggregate(p)] for name, p in pipelines.items()}
    # Spilled: the ties come in the order the runs' merge leaves them.
    knob = "internalQueryMaxBlockingSortMemoryUsageBytes"
    client.admin.command("setParameter", 1, **{knob: 6000})
    try:
        for name in ("after $addFields", "descending"):
            out[f"{name}, spilled"] = [
                d["_id"] for d in c.aggregate(pipelines[name], allowDiskUse=True)
            ]
    finally:
        client.admin.command("setParameter", 1, **{knob: 104857600})
    return out


@pytest.mark.backlog_fixed("RM-527")
def test_tied_order_of_find_leading_sort_and_window(rust_mongo: Any, mongod: Any) -> None:
    """mongod sorts all three with its sorter's `std::sort`, so the 40
    documents over three values of `k` come back reordered -- macOS mongod
    8.2.11: 39, 3, 27, 9, ...; libstdc++'s order on Linux: 0, 21, 18, 24,
    ... (g++ 15.2) -- where this server's `$setWindowFields` returned them in
    the order read. `find` and a leading `$sort` agreed since RM-312 (Linux)
    and RM-526 (macOS), and the window since RM-527, which hands it the
    documents in the order its budgeted sort leaves them."""
    assert _tied_order(rust_mongo) == _tied_order(mongod)


@pytest.mark.backlog_fixed("RM-527")
def test_window_ties_in_each_shape_and_when_spilled(rust_mongo: Any, mongod: Any) -> None:
    """The window's sort on its partition and `sortBy` reorders ties as
    mongod 8.2.11's does: a partition with no `sortBy`, both, descending,
    behind a `$match` or an `$addFields`, a computed partition, a `$limit`
    after, and a sort that spills, whose ties come as its runs merge.
    (Behind an `$unwind` mongod sorts before it: RM-640.)"""
    ours, theirs = _window_tied_orders(rust_mongo), _window_tied_orders(mongod)
    assert {k: v for k, v in ours.items() if v != theirs[k]} == {}, theirs
