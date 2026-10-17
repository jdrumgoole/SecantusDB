"""Gap tests for Rust MongoDB server items found on 2026-10-17 during the
fifth backlog cycle (measured against mongod 8.2.11)."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _seeded(client: Any, name: str) -> Any:
    db = client["rm184"]
    db.drop_collection(name)
    db[name].insert_many([{"_id": i, "a": 3 - i} for i in range(3)])
    return db[name]


KEYS = [
    "$nope",
    {"a": "$nope"},
    {"a": {"b": "$nope"}},
    {"a": "$nope", "b": "$a"},
    {"a": "$nope", "b": "$nope"},
    ["$nope"],
    {"$ifNull": ["$nope", "$nope"]},
    {"a": {"$meta": "recordId"}},
    {"$meta": "indexKey"},
]


def _group_keys(client: Any) -> list:
    coll = _seeded(client, "keys")
    return [
        mongo_answer(
            lambda k=k: sorted(str(d["_id"]) for d in coll.aggregate([{"$group": {"_id": k}}]))
        )
        for k in KEYS
    ]


@pytest.mark.backlog_fixed("RM-184")
def test_group_key_with_a_missing_value(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 groups a missing key under null, and the missing field
    of a ONE-field document key under `{a: null}`; a document key of
    several fields drops a missing one (`{a: "$nope", b: "$a"}` groups under
    `{b: ..}`)."""
    assert _group_keys(rust_mongo) == _group_keys(mongod)


ARGS = ["$nope", {"$ifNull": ["$nope", "$nope"]}, {"$meta": "recordId"}, "$a"]


def _accumulated(client: Any) -> list:
    coll = _seeded(client, "acc")
    out = []
    for a in ARGS:
        pipeline = [
            {"$sort": {"_id": 1}},
            {
                "$group": {
                    "_id": None,
                    "p": {"$push": a},
                    "s": {"$addToSet": a},
                    "f": {"$first": a},
                    "l": {"$last": a},
                }
            },
        ]
        out.append(
            mongo_answer(
                lambda p=pipeline: [
                    (d["p"], sorted(d["s"], key=str), d["f"], d["l"]) for d in coll.aggregate(p)
                ]
            )
        )
    return out


@pytest.mark.backlog_fixed("RM-184")
def test_accumulating_a_missing_value(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `$push` / `$addToSet` skip an expression that is
    missing (`$ifNull` of two missing fields, a `$meta` with nothing to
    give), and `$first` / `$last` give null for it."""
    assert _accumulated(rust_mongo) == _accumulated(mongod)


SK = {"$meta": "sortKey"}
GD = {"$meta": "geoNearDistance"}
GP = {"$meta": "geoNearPoint"}
NEAR = {"$geoNear": {"near": {"type": "Point", "coordinates": [0, 0]}, "key": "loc"}}
SHAPES = [
    [{"$sort": {"a": 1}}, {"$project": {"k": SK}}],
    [{"$sort": {"b": 1, "a": -1}}, {"$addFields": {"k": SK}}],
    [{"$sort": {"a": 1}}, {"$match": {"a": {"$gte": 0}}}, {"$limit": 2}, {"$project": {"k": SK}}],
    [{"$sort": {"a": 1}}, {"$project": {"a": 1}}, {"$project": {"k": SK}}],
    [{"$sort": {"a": 1}}, {"$set": {"x": [1, 2]}}, {"$unwind": "$x"}, {"$project": {"k": SK}}],
    [{"$sort": {"a": 1}}, {"$group": {"_id": None, "k": {"$push": SK}}}],
    [{"$addFields": {"k": SK}}],
    [NEAR, {"$project": {"d": GD, "p": GP, "k": SK}}],
    [NEAR, {"$match": {"a": {"$gte": 0}}}, {"$sort": {"a": 1}}, {"$project": {"d": GD}}],
    [NEAR, {"$group": {"_id": None, "d": {"$push": GD}}}],
    [
        {
            "$geoNear": {
                "near": {"type": "Point", "coordinates": [0, 0]},
                "key": "loc",
                "distanceField": "dist",
                "includeLocs": "where",
            }
        },
        {"$project": {"d": GD, "dist": 1, "where": 1, "p": GP}},
    ],
]


def _meta_shapes(client: Any) -> list:
    db = client["rm083b"]
    db.drop_collection("c")
    db.c.create_index([("loc", "2dsphere")])
    db.c.insert_many(
        [
            {"_id": i, "a": 3 - i, "b": i % 2, "loc": {"type": "Point", "coordinates": [i, 0]}}
            for i in range(3)
        ]
    )
    return [mongo_answer(lambda p=p: list(db.c.aggregate(p))) for p in SHAPES]


@pytest.mark.backlog_fixed("RM-083")
def test_sort_key_and_geo_near_metadata_through_stages(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps a `$sort`'s key, and a `$geoNear`'s distance and
    point, through `$match`, `$limit`, `$project`, `$addFields`, `$unwind`
    and into a `$group`; with no `$sort`, `$addFields` gives `[]`."""
    assert _meta_shapes(rust_mongo) == _meta_shapes(mongod)


def _dbpointer_doc(key: str, ns: str, with_id: bool = False) -> Any:
    import struct

    from bson.raw_bson import RawBSONDocument

    raw_ns = ns.encode() + b"\x00"
    elems = (b"\x10_id\x00" + struct.pack("<i", 1)) if with_id else b""
    elems += b"\x0c" + key.encode() + b"\x00" + struct.pack("<i", len(raw_ns)) + raw_ns
    elems += bytes(12)
    return RawBSONDocument(struct.pack("<i", 4 + len(elems) + 1) + elems + b"\x00")


def _updates_over_a_dbpointer(client: Any) -> list:
    coll = client["rm127d"]["c"]
    out = []
    for upd in [
        {"$set": {"b": 1}},
        {"$min": {"a": 5}},
        {"$max": {"a": 5}},
        {"$min": _dbpointer_doc("a", "db.b")},
        {"$max": _dbpointer_doc("a", "db.b")},
        {"$min": _dbpointer_doc("a", "db.aa")},
        {"$set": _dbpointer_doc("a", "db.q")},
        {"$set": {"a": "s"}},
    ]:
        coll.drop()
        coll.insert_one(_dbpointer_doc("a", "db.z", with_id=True))
        out.append(
            mongo_answer(
                lambda u=upd: (coll.update_one({"_id": 1}, u).modified_count, str(coll.find_one()))
            )
        )
    return out


@pytest.mark.backlog_fixed("RM-127")
def test_updating_a_document_that_holds_a_dbpointer(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 updates a document holding a DBPointer like any other,
    and compares two DBPointers by the size of the value, then its bytes
    (a shorter namespace first, then the namespace, then the id). Every
    such update used to be refused here, `$set` of another field included."""
    assert _updates_over_a_dbpointer(rust_mongo) == _updates_over_a_dbpointer(mongod)
