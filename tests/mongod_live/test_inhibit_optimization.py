"""`$_internalInhibitOptimization`, against mongod 8.2 (RM-518).

The stage passes each document on as it is. What it is for is the
optimizer: mongod moves no stage across it and lets none behind it into the
query, so `[{$_internalInhibitOptimization: {}}, {$match: ...}]` reads the
whole collection and filters after, while a `$match` ahead of it is still
the query's. It takes an empty object (14 for anything but an object, 9 for
an object with a field), and an update may not run it (72).

mongod's dependency analysis stops at it too: a sort key read only behind
it is never asked for, so neither the read nor any `$sort` writes one and
the documents carry none at all (`{$meta: "sortKey"}` missing, a rank
refused with 9973200). A reader ahead of it asks; a `$geoNear` sets the key
whatever is asked.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import mongo_answer, same

STAGE = "$_internalInhibitOptimization"
INHIBIT = {STAGE: {}}
SORT_KEY = {"$meta": "sortKey"}
META = {"$addFields": {"m": SORT_KEY}}
RANK = {"$_internalSetWindowFields": {"sortBy": {"n": -1}, "output": {"r": {"$rank": {}}}}}

DOCS = [{"_id": i, "a": i % 3, "n": i, "t": [1, 2]} for i in range(6)]

ANSWERS: dict[str, list] = {
    "alone": [INHIBIT],
    "ahead of a $sort and a $limit": [INHIBIT, {"$sort": {"n": -1}}, {"$limit": 2}],
    "behind a $match": [{"$match": {"a": 1}}, INHIBIT],
    "ahead of a $match": [INHIBIT, {"$match": {"a": 1}}],
    "twice": [INHIBIT, {"$limit": 3}, INHIBIT],
    "ahead of a $group": [
        INHIBIT,
        {"$group": {"_id": "$a", "c": {"$sum": 1}}},
        {"$sort": {"_id": 1}},
    ],
    "a sort key read only behind it": [{"$sort": {"n": -1}}, INHIBIT, META],
    "a $sort behind it writes no key": [INHIBIT, {"$sort": {"n": -1}}, META],
    "a $sort behind each": [INHIBIT, {"$sort": {"n": -1}}, INHIBIT, META],
    "a key read only behind it, in a $project": [
        {"$sort": {"n": -1}},
        INHIBIT,
        {"$project": {"m": SORT_KEY}},
    ],
    "a key read only behind it, in a $group": [
        INHIBIT,
        {"$sort": {"n": 1}},
        {"$group": {"_id": SORT_KEY}},
    ],
    "a rank behind it": [{"$sort": {"n": -1}}, INHIBIT, RANK],
    "a reader ahead asks for the key": [
        {"$sort": {"n": -1}},
        META,
        INHIBIT,
        {"$addFields": {"k": SORT_KEY}},
    ],
    "a rank ahead asks for the key": [{"$sort": {"n": -1}}, RANK, INHIBIT, META],
    "a $lookup's reader asks only for its own": [
        {"$sort": {"n": -1}},
        {
            "$lookup": {
                "from": "c",
                "pipeline": [{"$sort": {"n": 1}}, META, {"$limit": 1}],
                "as": "j",
            }
        },
        INHIBIT,
        META,
    ],
    "a $lookup sub-pipeline's key read behind it": [
        {"$limit": 1},
        {"$lookup": {"from": "c", "pipeline": [INHIBIT, {"$sort": {"n": -1}}, META], "as": "j"}},
    ],
    "a $setWindowFields behind it ranks": [
        INHIBIT,
        {"$setWindowFields": {"sortBy": {"n": -1}, "output": {"r": {"$rank": {}}}}},
    ],
    "in a $facet": [{"$facet": {"x": [INHIBIT, {"$count": "n"}]}}],
    "in a $lookup": [
        {"$limit": 1},
        {"$lookup": {"from": "c", "pipeline": [INHIBIT, {"$limit": 1}], "as": "j"}},
    ],
    "in a $unionWith": [
        {"$limit": 1},
        {"$unionWith": {"coll": "c", "pipeline": [INHIBIT, {"$limit": 1}]}},
    ],
}

REFUSED_SPECS: dict[str, Any] = {
    "a number": 1,
    "a string": "x",
    "an array": [1, 2],
    "null": None,
    "a boolean": True,
    "a field": {"a": 1},
    "fields": {"a": {"b": [1, "x"]}, "c": "s"},
}

# The listing as `explain` gives it. A `$sort` with the `$limit` after it
# is left out: mongod lists the two as one `$sort` (RM-538).
EXPLAINED: dict[str, list] = {
    "a $match ahead stays the query's": [{"$match": {"a": 1}}, INHIBIT, {"$sort": {"n": -1}}],
    "a $match behind stays where it is": [INHIBIT, {"$match": {"a": 1}}],
    "a $match on each side": [{"$match": {"a": 1}}, INHIBIT, {"$match": {"n": 1}}],
    "a $project behind": [INHIBIT, {"$project": {"n": 1}}],
    "a $skip and a $limit behind": [{"$project": {"n": 1}}, INHIBIT, {"$skip": 1}, {"$limit": 2}],
    "a $sort ahead": [{"$sort": {"n": 1}}, INHIBIT, {"$limit": 2}],
}


def _load(client: Any) -> Any:
    client.drop_database("inhibit_optimization")
    db = client.inhibit_optimization
    db.c.insert_many(DOCS)
    db.c.create_index("a")
    return db


def _geo(client: Any) -> Any:
    db = _load(client)
    db.g.insert_many([{"_id": i, "loc": [i, i]} for i in range(3)])
    db.g.create_index([("loc", "2d")])
    return mongo_answer(
        lambda: list(
            db.g.aggregate([{"$geoNear": {"near": [0, 0], "distanceField": "d"}}, INHIBIT, META])
        )
    )


def test_a_geo_near_key_goes_through(rust_mongo: Any, mongod: Any) -> None:
    same(_geo(rust_mongo), _geo(mongod))


@pytest.mark.parametrize("name", list(ANSWERS))
def test_answers_as_without_it(rust_mongo: Any, mongod: Any, name: str) -> None:
    pipeline = ANSWERS[name]
    same(
        mongo_answer(lambda: list(_load(rust_mongo).c.aggregate(pipeline))),
        mongo_answer(lambda: list(_load(mongod).c.aggregate(pipeline))),
    )


@pytest.mark.parametrize("name", list(REFUSED_SPECS))
def test_takes_an_empty_object(rust_mongo: Any, mongod: Any, name: str) -> None:
    pipeline = [{STAGE: REFUSED_SPECS[name]}]
    same(
        mongo_answer(lambda: list(_load(rust_mongo).c.aggregate(pipeline))),
        mongo_answer(lambda: list(_load(mongod).c.aggregate(pipeline))),
    )


def _updates(client: Any) -> list:
    db = _load(client)
    write = db.command(
        {"update": "c", "updates": [{"q": {"_id": 0}, "u": [INHIBIT]}]},
    )["writeErrors"]
    return [
        [(e["code"], e["errmsg"]) for e in write],
        mongo_answer(
            lambda: db.command({"findAndModify": "c", "query": {"_id": 0}, "update": [INHIBIT]})
        ),
    ]


def test_an_update_may_not_run_it(rust_mongo: Any, mongod: Any) -> None:
    same(_updates(rust_mongo), _updates(mongod))


def _listing(client: Any, pipeline: list) -> list:
    stages = _load(client).command(
        {
            "explain": {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            "verbosity": "queryPlanner",
        }
    )["stages"]
    out = []
    for stage in stages:
        if "$cursor" in stage:
            planner = stage["$cursor"]["queryPlanner"]
            out.append({"$cursor": planner["parsedQuery"], "plan": planner["winningPlan"]})
        else:
            out.append(stage)
    return out


@pytest.mark.parametrize("name", list(EXPLAINED))
def test_explain_lists_it_where_it_was_written(rust_mongo: Any, mongod: Any, name: str) -> None:
    pipeline = EXPLAINED[name]
    same(_listing(rust_mongo, pipeline), _listing(mongod, pipeline))
