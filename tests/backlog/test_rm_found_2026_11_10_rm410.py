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


@pytest.mark.backlog_fixed("RM-520")
def test_the_sort_key_passes_through_stages_that_keep_it(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps a document's sort key through a `$replaceRoot`,
    a `$lookup` and a `$setWindowFields` with no `sortBy`, and a
    `$setWindowFields` that sorts sets its own (the partition key, then
    `sortBy`; a computed partition's value, and none for a constant one).
    This server ended the key at each, and so ranked every document 1."""
    meta = {"$addFields": {"m": {"$meta": "sortKey"}}}
    pipelines = {
        "$replaceRoot": [
            {"$sort": {"k": 1}},
            {"$replaceRoot": {"newRoot": "$$ROOT"}},
            RANK,
        ],
        "$replaceWith": [
            {"$sort": {"k": 1}},
            {"$replaceWith": {"x": "$n", "s": {"$meta": "sortKey"}}},
            meta,
        ],
        "$lookup with a pipeline": [
            {"$sort": {"k": 1}},
            {"$lookup": {"from": "c", "pipeline": [{"$limit": 1}], "as": "j"}},
            {"$project": {"j": 0}},
            meta,
        ],
        "computed partition": [
            {
                "$setWindowFields": {
                    "partitionBy": {"$mod": ["$n", 2]},
                    "sortBy": {"_id": 1},
                    "output": {"x": {"$sum": 1}},
                }
            },
            meta,
            RANK,
        ],
        "constant partition": [
            {
                "$setWindowFields": {
                    "partitionBy": {"$literal": "x"},
                    "sortBy": {"_id": 1},
                    "output": {"x": {"$sum": 1}},
                }
            },
            meta,
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


@pytest.mark.backlog_fixed("RM-521")
def test_a_lookup_sub_pipeline_sort_sets_the_key(rust_mongo: Any, mongod: Any) -> None:
    """In a `$lookup` sub-pipeline, mongod 8.2.11 ranks by the key its
    `$sort` set and gives it as `{$meta: "sortKey"}`; this server's
    sub-pipeline saw none and tied them all, until RM-521 rewrote the
    sub-pipeline's sort metadata as the top-level pipeline's is."""
    meta = {"$addFields": {"m": {"$meta": "sortKey"}}}
    pipelines = {
        "sub-pipeline": [
            {"$limit": 1},
            {"$lookup": {"from": "c", "pipeline": [{"$sort": {"k": 1}}, RANK], "as": "j"}},
        ],
        "$meta": [
            {"$limit": 1},
            {"$lookup": {"from": "c", "pipeline": [{"$sort": {"k": 1, "n": -1}}, meta], "as": "j"}},
        ],
        "join fields and a pipeline": [
            {"$limit": 2},
            {
                "$lookup": {
                    "from": "c",
                    "localField": "k",
                    "foreignField": "k",
                    "pipeline": [{"$sort": {"n": -1}}, RANK, meta],
                    "as": "j",
                }
            },
        ],
        "no $sort": [
            {"$limit": 1},
            {"$lookup": {"from": "c", "pipeline": [RANK, meta], "as": "j"}},
        ],
        "$match after the $sort": [
            {"$limit": 1},
            {
                "$lookup": {
                    "from": "c",
                    "pipeline": [
                        {"$sort": {"k": 1}},
                        {"$match": {"$expr": {"$eq": [{"$meta": "sortKey"}, None]}}},
                    ],
                    "as": "j",
                }
            },
        ],
        "nested $lookup": [
            {"$limit": 1},
            {
                "$lookup": {
                    "from": "c",
                    "pipeline": [
                        {"$limit": 1},
                        {
                            "$lookup": {
                                "from": "c",
                                "pipeline": [{"$sort": {"k": 1}}, RANK],
                                "as": "jj",
                            }
                        },
                    ],
                    "as": "j",
                }
            },
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog_fixed("RM-522")
def test_a_rank_with_no_sort_key_is_refused(rust_mongo: Any, mongod: Any) -> None:
    """After a `$group` or a `$unionWith` a document has no sort key at all
    (`{$meta: "sortKey"}` is missing, not `[]`), and mongod 8.2.11 refuses
    the rank: 9973200 "Attempt to get sort key which has not been
    populated". This server tied them, until RM-522 marked such documents
    and refused the rank that reaches one."""
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


@pytest.mark.backlog_fixed("RM-523")
def test_which_sort_sets_the_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 takes only the `$sort` its query layer runs as the
    documents' key: behind a leading `$sort` and an `$addFields` the key
    stays the first sort's, and a `$sort` behind a leading `$addFields` or
    `$project` leaves the empty key -- for `{$meta: "sortKey"}` and the
    ranks alike. A `$sort` the pipeline runs sorts and sets no key. This
    server took the last `$sort`'s."""
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


@pytest.mark.backlog_fixed("RM-523")
def test_a_sort_the_pipeline_runs_keeps_the_key_it_was_handed(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 (measured 2026-11-13): a `$sort` behind an `$unwind`
    that leaves its fields alone moves ahead of it into the query layer and
    sets the key, one on the unwound path or behind an `$addFields` does
    not; a `$sort` behind a `$geoNear` keeps the distance as the key; a
    `$lookup` pipeline's `$sort` follows the same rule, a view's stages in
    front of it; a `$setWindowFields` behind another stage still sets its
    own."""
    meta = {"$addFields": {"m": {"$meta": "sortKey"}}}
    unwind = {"$unwind": "$a"}
    window = {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"x": {"$sum": 1}}}}

    def sub(pipeline: list) -> list:
        return [
            {"$limit": 1},
            {"$lookup": {"from": "c", "pipeline": [*pipeline, meta], "as": "j"}},
            {"$project": {"j.m": 1}},
        ]

    pipelines = {
        "$unwind, $sort": [unwind, {"$sort": {"n": 1}}, meta, RANK],
        "$sort, $unwind, $sort": [{"$sort": {"k": 1}}, unwind, {"$sort": {"n": 1}}, meta],
        "$unwind, $sort on the path": [unwind, {"$sort": {"a": 1}}, meta],
        "$addFields, $unwind, $sort": [{"$addFields": {"q": 1}}, unwind, {"$sort": {"n": 1}}, meta],
        "$addFields, $setWindowFields": [{"$addFields": {"q": 1}}, window, meta],
        "$lookup: $addFields, $sort": sub([{"$addFields": {"q": 1}}, {"$sort": {"n": 1}}]),
        "$lookup: $sort, $addFields, $sort": sub(
            [{"$sort": {"k": 1}}, {"$addFields": {"q": 1}}, {"$sort": {"n": 1}}]
        ),
    }

    def answers(client: Any) -> dict:
        # Ties too come in mongod's order, since RM-524.
        out = _answers(client, pipelines)
        db = client.rmfound1110sk
        db.g.insert_many([{"_id": i, "loc": [i, 0], "n": 5 - i} for i in range(4)])
        db.g.create_index([("loc", "2d")])
        near = {"$geoNear": {"near": [0, 0], "distanceField": "d"}}
        out["$geoNear, $sort"] = mongo_answer(
            lambda: list(
                db.g.aggregate([near, {"$addFields": {"q": 1}}, {"$sort": {"n": 1}}, meta])
            )
        )
        db.command("create", "v", viewOn="c", pipeline=[{"$addFields": {"q": 1}}])
        through_view = sub([{"$sort": {"n": 1}}])
        through_view[1]["$lookup"]["from"] = "v"
        out["$lookup of a view: $sort"] = mongo_answer(lambda: list(db.c.aggregate(through_view)))
        return out

    assert answers(rust_mongo) == answers(mongod)


@pytest.mark.backlog_fixed("RM-524")
def test_a_sort_before_another_sort_is_dropped(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 drops a `$sort` that a later `$sort` overrides, also
    across an `$unwind`, and the one a sorting `$setWindowFields` runs
    behind: ties under the second come in the collection's order (`_id` 2
    before 3), not the first sort's (3 before 2). This server ran both, and
    the ties kept the first sort's order, until RM-524 dropped the earlier
    `$sort` as mongod's optimizer does."""
    pipelines = {
        "$sort, $sort": [{"$sort": {"k": 1}}, {"$sort": {"n": 1}}, {"$project": {"_id": 1}}],
        "$sort, $setWindowFields": [
            {"$sort": {"k": 1}},
            {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"x": {"$sum": 1}}}},
            {"$project": {"_id": 1}},
        ],
        "$sort, $unwind, $sort": [
            {"$sort": {"k": 1}},
            {"$unwind": "$a"},
            {"$sort": {"n": 1}},
            {"$project": {"_id": 1}},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog_fixed("RM-524")
def test_what_lies_between_two_sorts(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 drops the earlier `$sort` across the stages its
    optimizer moves out from between the two -- a `$match` (ahead of an
    `$unwind` too, when it reads nothing the `$unwind` writes), a `$skip`
    of 0, an `$unwind` the later `$sort` does not read -- and behind a
    `$setWindowFields` partitioned by a field; in `$facet` and `$lookup`
    sub-pipelines alike. A `$limit` or an `$addFields` between keeps both,
    as does a `$match` on the unwound field, an `$unwind` the later `$sort`
    reads, and a computed partition, which mongod sets with an `$addFields`
    ahead of the window's `$sort` (measured 2026-11-13, RM-524)."""
    ids = {"$project": {"_id": 1}}
    pipelines = {
        "$match": [{"$sort": {"k": 1}}, {"$match": {"n": {"$gt": 0}}}, {"$sort": {"n": 1}}, ids],
        "$skip 0": [{"$sort": {"k": 1}}, {"$skip": 0}, {"$sort": {"n": 1}}, ids],
        "three $sorts": [{"$sort": {"k": -1}}, {"$sort": {"k": 1}}, {"$sort": {"n": 1}}, ids],
        "$unwind, $match": [
            {"$sort": {"k": 1}},
            {"$unwind": {"path": "$a", "includeArrayIndex": "i"}},
            {"$match": {"n": {"$gt": 0}}},
            {"$sort": {"n": 1}},
            ids,
        ],
        "partition by a field": [
            {"$sort": {"k": 1}},
            {"$setWindowFields": {"partitionBy": "$n", "output": {"x": {"$sum": 1}}}},
            ids,
        ],
        "$facet": [{"$facet": {"f": [{"$sort": {"k": 1}}, {"$sort": {"n": 1}}, ids]}}],
        "$lookup": [
            {"$limit": 1},
            {
                "$lookup": {
                    "from": "c",
                    "pipeline": [{"$sort": {"k": 1}}, {"$sort": {"n": 1}}, ids],
                    "as": "j",
                }
            },
            {"$project": {"j": 1}},
        ],
        "$limit keeps both": [{"$sort": {"k": 1}}, {"$limit": 9}, {"$sort": {"n": 1}}, ids],
        "$addFields keeps both": [
            {"$sort": {"k": 1}},
            {"$addFields": {"q": 1}},
            {"$sort": {"k": -1}},
            {"$sort": {"n": 1}},
            ids,
        ],
        "$match on the unwound field keeps both": [
            {"$sort": {"k": 1}},
            {"$unwind": "$a"},
            {"$match": {"a": {"$gt": 0}}},
            {"$sort": {"n": 1}},
            ids,
        ],
        "sort on the unwound index keeps both": [
            {"$sort": {"k": -1}},
            {"$unwind": {"path": "$a", "includeArrayIndex": "i"}},
            {"$sort": {"i": 1}},
            {"$project": {"_id": 1, "i": 1}},
        ],
        "computed partition keeps both": [
            {"$sort": {"k": 1}},
            {
                "$setWindowFields": {
                    "partitionBy": {"$mod": ["$n", 2]},
                    "sortBy": {"n": 1},
                    "output": {"x": {"$sum": 1}},
                }
            },
            ids,
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog_fixed("RM-524")
def test_a_views_sort_is_overridden(rust_mongo: Any, mongod: Any) -> None:
    """A `$sort` that ends a view's pipeline is dropped behind the caller's
    `$sort` (and its `$match`), as in one pipeline, by mongod 8.2.11; one a
    `$limit` follows is kept (measured 2026-11-13, RM-524)."""

    def answers(client: Any) -> dict:
        _answers(client, {})
        db = client.rmfound1110sk
        db.command("create", "v", viewOn="c", pipeline=[{"$sort": {"k": 1}}])
        db.command("create", "w", viewOn="c", pipeline=[{"$sort": {"k": 1}}, {"$limit": 9}])
        tail = [{"$match": {"n": {"$gt": 0}}}, {"$sort": {"n": 1}}, {"$project": {"_id": 1}}]
        return {
            name: mongo_answer(lambda name=name: list(db[name].aggregate(tail)))
            for name in ("v", "w")
        }

    assert answers(rust_mongo) == answers(mongod)


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
