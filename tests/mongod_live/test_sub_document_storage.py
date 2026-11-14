"""A sub-document's storage in the sizes a pipeline `$sort` and a window
count, against mongod 8.2 (RM-515).

mongod 8.2's classic engine gives a sub-document a storage of its own: a
stage that reads a field inside it caches that field there, and one that
writes into it sets the field on a copy. Each pipeline here runs ahead of a
`$setWindowFields`, whose `$sort` and cache count every document's storage;
the comparison is of the two counters `explain` reports.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import same

WINDOW = {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"z": {"$first": "$n"}}}}

PIPELINES = {
    "two fields set in one sub-document": [{"$set": {"e.z": 1, "e.w": "k" * 40}}],
    "a field set in a missing sub-document": [{"$set": {"f.z": 1}}],
    "a field set in a scalar": [{"$set": {"a.z": 1}}],
    "a field of a sub-document overwritten": [{"$set": {"e.y": "short"}}],
    "a field set two levels down": [{"$set": {"e.g.k": 1}}],
    "a field read two levels down": [{"$addFields": {"q": "$e.g.h"}}],
    "two reads in one sub-document": [{"$addFields": {"q": "$e.x", "r": "$e.y"}}],
    "a read after the copy, of a field not cached": [{"$addFields": {"r": 1, "q": "$e.y"}}],
    "a read after the copy, of a field cached before it": [
        {"$addFields": {"q": "$e"}},
        {"$addFields": {"r": 1, "s": "$e.y"}},
    ],
    "a sub-document set to a field, then read inside": [
        {"$addFields": {"q": "$e"}},
        {"$addFields": {"s": "$e.y"}},
    ],
    "a nested sub-document set to a field, then read inside": [
        {"$addFields": {"q": "$e.g"}},
        {"$addFields": {"r": 1, "s": "$e.g.h"}},
    ],
    "a sub-document set to a field, then written into": [
        {"$addFields": {"q": "$e"}},
        {"$set": {"e.z": 1}},
    ],
    "a read, then a write, in one sub-document": [
        {"$addFields": {"q": "$e.y"}},
        {"$set": {"e.z": 1}},
    ],
    "a sub-document a computed partition reads": [],
}


def _load(client: Any) -> Any:
    client.drop_database("rm515subdocs")
    db = client.rm515subdocs
    db.c.insert_many(
        [
            {
                "_id": i,
                "a": 5,
                "e": {"x": 1, "y": "w" * 50, "g": {"h": "v" * 30, "k": 2}},
                "n": i,
            }
            for i in range(200)
        ]
    )
    return db


def _sizes(client: Any, name: str) -> tuple:
    db = _load(client)
    window = WINDOW
    if name == "a sub-document a computed partition reads":
        window = {
            "$setWindowFields": {
                "partitionBy": {"$toUpper": "$e.y"},
                "sortBy": {"n": 1},
                "output": {"z": {"$first": "$n"}},
            }
        }
    pipeline = [*PIPELINES[name], window]
    ex = db.command(
        {
            "explain": {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            "verbosity": "executionStats",
        }
    )
    stages = ex["stages"]
    sort = next(s for s in stages if "$sort" in s)
    win = next(s for s in stages if "$_internalSetWindowFields" in s)
    return sort["totalDataSizeSortedBytesEstimate"], win["maxTotalMemoryUsageBytes"]


@pytest.mark.parametrize("name", list(PIPELINES))
def test_a_sub_document_is_sized_by_its_storage(rust_mongo: Any, mongod: Any, name: str) -> None:
    same(_sizes(rust_mongo, name), _sizes(mongod, name))
