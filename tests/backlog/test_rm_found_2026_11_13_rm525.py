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


@pytest.mark.backlog("RM-635")
def test_a_stored_undefined_is_not_missing(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$type` of a stored `undefined` reads "undefined",
    read as a field, an array element, a `$let` variable or a field an
    `$addFields` copied it to. This server reads "missing" in each."""
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


@pytest.mark.backlog("RM-635")
def test_a_group_keeps_undefined_apart_from_null(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$group` keeps a stored `undefined` key apart from
    null and missing, and groups on an array holding `undefined`. This
    server folds the undefined key into null's group, and refuses the
    array key (code 2, "not supported by the Rust server")."""
    pipelines = {
        "scalar key": [
            {"$group": {"_id": "$a", "n": {"$sum": 1}}},
            {"$project": {"_id": {"$type": "$_id"}, "n": 1}},
            {"$sort": {"_id": 1}},
        ],
        "array key": [{"$group": {"_id": "$b"}}, {"$count": "groups"}],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)
