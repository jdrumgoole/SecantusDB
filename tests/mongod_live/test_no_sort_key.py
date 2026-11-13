"""A document with no sort key at all, against mongod 8.2 (RM-522).

Behind a `$group` (and the stages that make new documents as it does) no
document has a sort key -- not the empty one a read with no `$sort` gives,
but none -- and a `$sort` behind it gives them none either. The documents a
`$unionWith` adds have none when a rank reads them first. mongod's
`$_internalSetWindowFields` refuses a `$rank`, `$denseRank` or
`$documentNumber` on such a document with 9973200 as it reaches it, so a
`$limit` that stops first answers; `{$meta: "sortKey"}` is missing there.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import mongo_answer, same
from tests.mongod_live.test_internal_window_sort_key import DOCS


def _window(output: dict, **extra: Any) -> dict:
    return {"$_internalSetWindowFields": {"sortBy": {"_id": 1}, "output": output, **extra}}


RANK = _window({"r": {"$rank": {}}})
META = {"$addFields": {"m": {"$meta": "sortKey"}}}
GROUP = {"$group": {"_id": "$k"}}
BY_ID = {"$sort": {"_id": 1}}

CASES: dict[str, list] = {
    "$group": [GROUP, RANK],
    "$group, partitioned": [GROUP, _window({"r": {"$rank": {}}}, partitionBy="$_id")],
    "$group, $denseRank": [GROUP, _window({"r": {"$denseRank": {}}})],
    "$group, $documentNumber": [GROUP, _window({"r": {"$documentNumber": {}}})],
    "$group, a window that numbers nothing": [
        GROUP,
        BY_ID,
        _window({"s": {"$sum": 1}}),
    ],
    "$group behind a $sort, then a $sort": [{"$sort": {"k": 1}}, GROUP, BY_ID, RANK],
    "$group, $unwind, $sort": [
        {"$group": {"_id": "$k", "a": {"$push": "$n"}}},
        {"$unwind": "$a"},
        {"$sort": {"a": 1}},
        RANK,
    ],
    "$group with nothing out": [{"$match": {"n": 99}}, GROUP, RANK],
    "$group, a $limit": [GROUP, RANK, {"$limit": 1}],
    "$bucket": [{"$bucket": {"groupBy": "$n", "boundaries": [0, 3, 10]}}, RANK],
    "$bucketAuto": [{"$bucketAuto": {"groupBy": "$n", "buckets": 2}}, RANK],
    "$sortByCount": [{"$sortByCount": "$k"}, RANK],
    "$count": [{"$count": "x"}, RANK],
    "$facet": [{"$facet": {"a": [{"$limit": 1}]}}, RANK],
    "$redact": [{"$sort": {"k": 1}}, {"$redact": "$$KEEP"}, RANK],
    "$unionWith": [{"$unionWith": "c"}, RANK],
    "$unionWith, a sub-pipeline $sort": [
        {"$sort": {"k": 1}},
        {"$unionWith": {"coll": "c", "pipeline": [{"$sort": {"n": 1}}]}},
        RANK,
    ],
    "$unionWith of $documents": [
        {"$unionWith": {"pipeline": [{"$documents": [{"_id": 9}]}]}},
        RANK,
    ],
    "$unionWith, a $sort": [{"$unionWith": "c"}, {"$sort": {"n": 1}}, RANK],
    "$unionWith, an inclusion $project": [
        {"$unionWith": "c"},
        {"$project": {"n": 1}},
        RANK,
    ],
    "$unionWith, a $replaceWith": [
        {"$unionWith": "c"},
        {"$replaceWith": {"_id": "$_id", "x": "$n"}},
        RANK,
    ],
    "$meta behind a $group, in a $lookup sub-pipeline": [
        GROUP,
        {"$lookup": {"from": "c", "pipeline": [{"$sort": {"n": 1}}, META], "as": "j"}},
        BY_ID,
    ],
    "$unionWith, a $meta first": [{"$unionWith": "c"}, META, RANK],
    "$unionWith, a $limit before its documents": [
        {"$unionWith": "c"},
        RANK,
        {"$limit": 6},
    ],
    "$unionWith of an empty collection": [{"$unionWith": "none"}, RANK],
    "$meta behind a $group": [GROUP, BY_ID, META],
    "$meta behind a $group, in a $project": [
        GROUP,
        BY_ID,
        {"$project": {"m": {"$meta": "sortKey"}}},
    ],
    "$meta behind a $group and a $sort": [{"$sort": {"k": 1}}, GROUP, BY_ID, META],
    "$meta in a $group behind a $group": [
        GROUP,
        {"$group": {"_id": {"$meta": "sortKey"}, "m": {"$push": {"$meta": "sortKey"}}}},
    ],
    "$meta behind a $redact": [{"$redact": "$$KEEP"}, META],
    "a $lookup sub-pipeline's $group": [
        {"$limit": 1},
        {"$lookup": {"from": "c", "pipeline": [GROUP, RANK], "as": "j"}},
    ],
}


def _load(client: Any) -> Any:
    client.drop_database("no_sort_key")
    db = client.no_sort_key
    db.c.insert_many(DOCS)
    return db


@pytest.mark.parametrize("name", list(CASES))
def test_no_sort_key(rust_mongo: Any, mongod: Any, name: str) -> None:
    pipeline = CASES[name]
    same(
        mongo_answer(lambda: list(_load(rust_mongo).c.aggregate(pipeline))),
        mongo_answer(lambda: list(_load(mongod).c.aggregate(pipeline))),
    )
