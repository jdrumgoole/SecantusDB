"""Gap tests for what `explain`'s memory counters showed once they were
listed (RM-328, 2026-11-08): sizes this server's memory model gets wrong,
and a corner of `$_internalSetWindowFields`, compared with a live mongod.

The counters compared do not depend on a function's own size, which
differs between mongod's builds (RM-397, RM-403): windows of `$first`,
which holds nothing of its own, and the differences between two
accumulators of one kind.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _load(client: Any) -> Any:
    client.drop_database("rmfound1108")
    db = client.rmfound1108
    db.c.insert_many([{"_id": i, "k": i % 7, "s": "x" * 1000, "n": i} for i in range(300)])
    return db


def _stages(db: Any, pipeline: list) -> list:
    ex = db.command(
        {
            "explain": {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            "verbosity": "executionStats",
        }
    )
    return ex["stages"][1:]


def _changed_document_sizes(client: Any) -> dict:
    db = _load(client)
    first = {"z": {"$first": "$n"}}
    window = _stages(
        db, [{"$unwind": "$s"}, {"$setWindowFields": {"sortBy": {"n": 1}, "output": first}}]
    )
    computed = _stages(
        db,
        [
            {
                "$setWindowFields": {
                    "partitionBy": {"$mod": ["$n", 3]},
                    "sortBy": {"n": 1},
                    "output": first,
                }
            }
        ],
    )
    group = _stages(db, [{"$unwind": "$s"}, {"$group": {"_id": {"a": "$k", "b": "$k"}}}])
    push = _stages(db, [{"$unwind": "$s"}, {"$group": {"_id": "$k", "a": {"$push": {"x": "$n"}}}}])
    return {
        "a window after an $unwind": window[-1]["maxTotalMemoryUsageBytes"],
        "the sort in front of a computed partition": computed[1][
            "totalDataSizeSortedBytesEstimate"
        ],
        "a window over a computed partition": computed[2]["maxTotalMemoryUsageBytes"],
        "a group's built _id": group[-1]["totalOutputDataSizeBytes"],
        "a $push of objects": push[-1]["maxAccumulatorMemoryUsageBytes"]["a"],
        "the groups holding them": push[-1]["totalOutputDataSizeBytes"],
    }


@pytest.mark.backlog("RM-408")
def test_changed_documents_are_sized_as_mongod_sizes_them(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.1 sizes a document an `$unwind` or `$addFields` changed,
    and one an expression object built, by its storage: what it allocated
    and what its new values hold, on top of the BSON it came from."""
    assert _changed_document_sizes(rust_mongo) == _changed_document_sizes(mongod)


def _first_holds(client: Any) -> int:
    db = _load(client)
    stages = _stages(
        db,
        [
            {"$unwind": "$s"},
            {"$group": {"_id": "$k", "long": {"$first": "$s"}, "int": {"$first": "$n"}}},
        ],
    )
    memory = stages[-1]["maxAccumulatorMemoryUsageBytes"]
    return memory["long"] - memory["int"]


@pytest.mark.backlog("RM-409")
def test_a_classic_first_counts_its_value(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.1's classic `$first` counts the value it holds: a
    1000-byte string's 1024 bytes beyond an int's, in each of 7 groups."""
    assert _first_holds(rust_mongo) == _first_holds(mongod)


def _internal_ranks(client: Any) -> Any:
    client.drop_database("rmfound1108r")
    db = client.rmfound1108r
    db.c.insert_many([{"_id": i, "n": [3, 1, 2, 2, 5][i]} for i in range(5)])
    runs = {
        "no sort": [
            {"$_internalSetWindowFields": {"sortBy": {"n": 1}, "output": {"r": {"$rank": {}}}}}
        ],
        "behind a $sort": [
            {"$sort": {"n": 1}},
            {"$_internalSetWindowFields": {"sortBy": {"n": 1}, "output": {"r": {"$rank": {}}}}},
        ],
    }
    return {name: mongo_answer(lambda p=p: list(db.c.aggregate(p))) for name, p in runs.items()}


@pytest.mark.backlog("RM-410")
def test_an_internal_window_ranks_by_the_sort_key_it_was_given(
    rust_mongo: Any, mongod: Any
) -> None:
    """A pipeline that names `$_internalSetWindowFields` itself: mongod
    8.2.1's rank functions read the sort key a `$sort` attached to each
    document, and without one every document ties."""
    assert _internal_ranks(rust_mongo) == _internal_ranks(mongod)
