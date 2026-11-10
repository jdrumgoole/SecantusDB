"""Gap tests for the sort key a document carries, found working RM-410
(2026-11-10): which stages pass it on, which `$sort` sets it, what it is
with none, and the order a `$sort` before another leaves.

mongod's `$rank` and `$denseRank` in a pipeline's own
`$_internalSetWindowFields` compare that key, and agree with `{$meta:
"sortKey"}` in every shape measured, so the ranks show it.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

DOCS = [
    {"_id": i, "n": n, "k": k, "a": a}
    for i, (n, k, a) in enumerate(
        [(3, 1, [2, 9]), (1, 0, [5]), (2, 1, [1, 3]), (2, 0, 7), (5, 1, []), (4, 1, None)]
    )
]

RANK = {"$_internalSetWindowFields": {"sortBy": {"n": 1}, "output": {"r": {"$rank": {}}}}}
RANK_ID = {"$_internalSetWindowFields": {"sortBy": {"_id": 1}, "output": {"r": {"$rank": {}}}}}


def _answers(client: Any, pipelines: dict[str, list]) -> dict:
    client.drop_database("rmfound1110sk")
    db = client.rmfound1110sk
    db.c.insert_many(DOCS)
    return {
        name: mongo_answer(lambda p=p: list(db.c.aggregate(p))) for name, p in pipelines.items()
    }


@pytest.mark.backlog("RM-520")
def test_the_sort_key_passes_through_stages_that_keep_it(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps a document's sort key through a `$replaceRoot`,
    a `$lookup` and a `$setWindowFields` with no `sortBy`, and a
    `$setWindowFields` that sorts sets its own (the partition key, then
    `sortBy`)."""
    pipelines = {
        "$replaceRoot": [
            {"$sort": {"k": 1}},
            {"$replaceRoot": {"newRoot": "$$ROOT"}},
            RANK,
        ],
        "$lookup": [
            {"$sort": {"k": 1}},
            {"$lookup": {"from": "c", "localField": "_id", "foreignField": "_id", "as": "j"}},
            {"$project": {"j": 0}},
            RANK,
        ],
        "unsorted $setWindowFields": [
            {"$sort": {"k": 1}},
            {"$setWindowFields": {"output": {"x": {"$sum": 1}}}},
            RANK,
        ],
        "sorted $setWindowFields": [
            {
                "$setWindowFields": {
                    "partitionBy": "$k",
                    "sortBy": {"n": 1},
                    "output": {"x": {"$sum": 1}},
                }
            },
            RANK,
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-521")
def test_a_lookup_sub_pipeline_sort_sets_the_key(rust_mongo: Any, mongod: Any) -> None:
    """In a `$lookup` sub-pipeline, mongod 8.2.11 ranks by the key its
    `$sort` set; this server's sub-pipeline sees none and ties them all."""
    pipelines = {
        "sub-pipeline": [
            {"$limit": 1},
            {"$lookup": {"from": "c", "pipeline": [{"$sort": {"k": 1}}, RANK], "as": "j"}},
        ]
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-522")
def test_a_rank_with_no_sort_key_is_refused(rust_mongo: Any, mongod: Any) -> None:
    """After a `$group` or a `$unionWith` a document has no sort key at all
    (`{$meta: "sortKey"}` is missing, not `[]`), and mongod 8.2.11 refuses
    the rank: 9973200 "Attempt to get sort key which has not been
    populated". This server ties them."""
    pipelines = {
        "$group": [{"$group": {"_id": "$k"}}, RANK_ID],
        "$group then $sort": [
            {"$sort": {"k": 1}},
            {"$group": {"_id": "$k"}},
            {"$sort": {"_id": -1}},
            RANK_ID,
        ],
        "$unionWith": [
            {"$sort": {"k": 1}},
            {"$unionWith": {"coll": "c", "pipeline": [{"$sort": {"n": 1}}]}},
            RANK,
        ],
    }
    ours, theirs = _answers(rust_mongo, pipelines), _answers(mongod, pipelines)
    assert {k: v[:2] for k, v in ours.items()} == {k: v[:2] for k, v in theirs.items()}


@pytest.mark.backlog("RM-523")
def test_which_sort_sets_the_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 does not take a `$sort` that runs behind another stage
    as the documents' key: behind a leading `$sort` and an `$addFields` the
    key stays the first sort's, and a `$sort` behind a leading `$addFields`
    or `$project` leaves the empty key -- for `{$meta: "sortKey"}` and the
    ranks alike. This server takes the last `$sort`'s."""
    meta = {"$addFields": {"m": {"$meta": "sortKey"}}}
    pipelines = {
        "$sort, $addFields, $sort": [
            {"$sort": {"k": 1}},
            {"$addFields": {"q": 1}},
            {"$sort": {"n": 1}},
            meta,
            RANK,
        ],
        "$addFields, $sort": [{"$addFields": {"q": 1}}, {"$sort": {"n": 1}}, meta, RANK],
        "$project, $sort": [{"$project": {"n": 1, "k": 1}}, {"$sort": {"k": 1}}, meta, RANK],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-524")
def test_a_sort_before_another_sort_is_dropped(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 drops a `$sort` that a later `$sort` overrides, also
    across an `$unwind`: ties under the second come in the collection's
    order (`_id` 2 before 3), not the first sort's (3 before 2)."""
    pipelines = {
        "$sort, $sort": [{"$sort": {"k": 1}}, {"$sort": {"n": 1}}, {"$project": {"_id": 1}}],
        "$sort, $unwind, $sort": [
            {"$sort": {"k": 1}},
            {"$unwind": "$a"},
            {"$sort": {"n": 1}},
            {"$project": {"_id": 1}},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-525")
def test_an_empty_arrays_sort_key_is_undefined(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's sort key for an empty array is `undefined` (pymongo
    decodes it as `None`, so `$type` and a `$group` show it): a `$group` on
    `{$meta: "sortKey"}` keeps the empty array's document and the null's
    apart. This server gives the empty array's key as null."""
    pipelines = {
        "$type": [
            {"$sort": {"a": 1}},
            {
                "$project": {
                    "t": {"$map": {"input": {"$meta": "sortKey"}, "in": {"$type": "$$this"}}}
                }
            },
        ],
        "$group": [
            {"$sort": {"a": 1}},
            {"$group": {"_id": {"$meta": "sortKey"}}},
            {"$count": "groups"},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)
