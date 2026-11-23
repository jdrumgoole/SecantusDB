"""Gap tests found working RM-525 (2026-11-13): a stored BSON `undefined`
in an aggregation expression.

The Rust server's expression engine spells MISSING `Bson::Undefined`, so a
real `undefined` value -- a stored field, an array element, the empty
array's part of a sort key -- reads as MISSING. mongod 8.2.11 keeps the two
apart. pymongo cannot encode `undefined`, so the documents are raw BSON.
"""

from __future__ import annotations

from typing import Any

import pytest
from bson.raw_bson import RawBSONDocument

from tests.backlog.conftest import mongo_answer


def _doc(*elements: bytes) -> RawBSONDocument:
    body = b"".join(elements)
    return RawBSONDocument((len(body) + 5).to_bytes(4, "little") + body + b"\x00")


def _int(name: str, v: int) -> bytes:
    return b"\x10" + name.encode() + b"\x00" + v.to_bytes(4, "little", signed=True)


def _undefined(name: str) -> bytes:
    return b"\x06" + name.encode() + b"\x00"


def _null(name: str) -> bytes:
    return b"\x0a" + name.encode() + b"\x00"


def _undefined_array(name: str) -> bytes:
    """`name: [undefined]`."""
    inner = b"\x060\x00"
    return (
        b"\x04" + name.encode() + b"\x00" + (len(inner) + 5).to_bytes(4, "little") + inner + b"\x00"
    )


# `a` is undefined, null and missing; `b` is `[undefined]` and missing.
DOCS = [
    _doc(_int("_id", 1), _undefined("a"), _undefined_array("b")),
    _doc(_int("_id", 2), _null("a")),
    _doc(_int("_id", 3)),
]

SORT = {"$sort": {"_id": 1}}


def _answers(client: Any, pipelines: dict[str, list]) -> dict:
    client.drop_database("rmfound1113rm525")
    db = client.rmfound1113rm525
    db.c.insert_many(DOCS)
    return {
        name: mongo_answer(lambda p=p: list(db.c.aggregate(p))) for name, p in pipelines.items()
    }


@pytest.mark.backlog_fixed("RM-635")
def test_a_stored_undefined_is_not_missing(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$type` of a stored `undefined` reads "undefined",
    read as a field, an array element, a `$let` variable or a field an
    `$addFields` copied it to. This server read "missing" in each (RM-635)."""
    pipelines = {
        "field": [SORT, {"$project": {"t": {"$type": "$a"}}}],
        "array element": [
            {"$match": {"_id": 1}},
            {"$project": {"t": {"$type": {"$arrayElemAt": ["$b", 0]}}}},
        ],
        "$map": [
            {"$match": {"_id": 1}},
            {"$project": {"t": {"$map": {"input": "$b", "in": {"$type": "$$this"}}}}},
        ],
        "$let": [
            SORT,
            {"$project": {"t": {"$let": {"vars": {"x": "$a"}, "in": {"$type": "$$x"}}}}},
        ],
        "$addFields": [SORT, {"$addFields": {"c": "$a"}}, {"$project": {"c": {"$type": "$c"}}}],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog_fixed("RM-635")
def test_a_group_keeps_undefined_apart_from_null(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$group` keeps a stored `undefined` key apart from
    null and missing, and groups on an array holding `undefined`. This
    server folded the undefined key into null's group, and refused the
    array key (code 2, "not supported by the Rust server"; RM-635)."""
    pipelines = {
        "scalar key": [
            {"$group": {"_id": "$a", "n": {"$sum": 1}}},
            {"$project": {"_id": {"$type": "$_id"}, "n": 1}},
            {"$sort": {"_id": 1}},
        ],
        "array key": [{"$group": {"_id": "$b"}}, {"$count": "groups"}],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog_fixed("RM-635")
def test_the_operators_that_pass_a_stored_undefined_on(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps a stored `undefined` as itself through `$cond`,
    `$switch`, `$getField`, `$first`, `$filter`, `$reduce`, a `$setField`
    value and an accumulator's `$first` / `$push` / `$addToSet`; `$ifNull`
    skips it as it skips null, as do `$min` and `$max` (RM-635)."""
    pipelines = {
        "$cond": [SORT, {"$project": {"t": {"$type": {"$cond": [True, "$a", 1]}}}}],
        "$getField": [SORT, {"$project": {"t": {"$type": {"$getField": "a"}}}}],
        "$first": [
            {"$match": {"_id": 1}},
            {"$project": {"t": {"$type": {"$first": "$b"}}}},
        ],
        "$reduce": [
            {"$match": {"_id": 1}},
            {
                "$project": {
                    "t": {
                        "$reduce": {
                            "input": "$b",
                            "initialValue": "",
                            "in": {"$concat": ["$$value", {"$type": "$$this"}, ","]},
                        }
                    }
                }
            },
        ],
        "$setField": [
            SORT,
            {"$project": {"t": {"$setField": {"field": "q", "input": {}, "value": "$a"}}}},
            {"$project": {"t": {"$type": "$t.q"}}},
        ],
        "$ifNull": [SORT, {"$project": {"t": {"$ifNull": ["$a", "dflt"]}}}],
        "$ifNull both": [SORT, {"$project": {"t": {"$type": {"$ifNull": ["$a", "$a"]}}}}],
        "$first accumulator": [
            SORT,
            {"$group": {"_id": None, "p": {"$first": "$a"}}},
            {"$project": {"p": {"$type": "$p"}}},
        ],
        "$addToSet": [
            SORT,
            {"$group": {"_id": None, "s": {"$addToSet": "$a"}}},
            # The set's own order is a hash's: sort the types.
            {
                "$project": {
                    "s": {
                        "$sortArray": {
                            "input": {"$map": {"input": "$s", "in": {"$type": "$$this"}}},
                            "sortBy": 1,
                        }
                    }
                }
            },
        ],
        "$max": [
            {"$group": {"_id": None, "p": {"$max": "$a"}}},
            {"$project": {"p": {"$type": "$p"}}},
        ],
        "$sortByCount": [
            {"$sortByCount": "$a"},
            {"$project": {"_id": {"$type": "$_id"}, "count": 1}},
            {"$sort": {"_id": 1}},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)
