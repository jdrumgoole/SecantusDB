"""A MinKey / MaxKey bound compares against a MISSING field too.

`$gt` / `$gte` / `$lt` / `$lte` are **type-bracketed**: `{x: {$gt: 3}}` matches
numbers greater than 3 and nothing else. `MinKey` and `MaxKey` are the two
bounds that escape that — mongod compares them against every type — and an
absent field is one of the things they must reach, because mongod's query
language treats a missing field as `null`, which ranks above `MinKey` and below
`MaxKey`.

Both servers skipped an absent field outright in the range comparison, so
`{x: {$gt: MinKey()}}` and `{x: {$lt: MaxKey()}}` left out every document with
no `x` at all — four shapes, measured against mongod 8.2.11 (2026-09-06).

The fix is to compare an absent field as `null` rather than skip it. That is
safe for the ordinary bounds precisely *because* they are bracketed: `{x: {$gt:
3}}` then compares `null` against a number, the brackets differ, and the
document is dropped exactly as before. The tests below pin both halves.
"""

from __future__ import annotations

import pymongo
import pytest
from bson import Binary, MaxKey, MinKey, ObjectId

from secantus import SecantusDBServer

OID = ObjectId("0123456789abcdef01234567")

DOCS = [
    {"_id": 1, "x": MinKey()},
    {"_id": 2, "x": None},
    {"_id": 3},  # MISSING — the document every one of these used to drop
    {"_id": 4, "x": 5},
    {"_id": 5, "x": "s"},
    {"_id": 6, "x": {}},
    {"_id": 7, "x": []},
    {"_id": 8, "x": [1]},
    {"_id": 9, "x": True},
    {"_id": 10, "x": MaxKey()},
    {"_id": 11, "x": Binary(b"\x01")},
    {"_id": 12, "x": OID},
]
ALL = [d["_id"] for d in DOCS]


@pytest.fixture
def coll(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    c = cli["minkeybounds"]["c"]
    c.insert_many([dict(d) for d in DOCS])
    try:
        yield c
    finally:
        cli.close()
        srv.stop()


def _ids(coll, q):
    return sorted(d["_id"] for d in coll.find(q))


# --- the bounds that compare against every type -----------------------------


@pytest.mark.parametrize(
    ("query", "expected"),
    [
        # Everything except the MinKey itself — the absent document included.
        ({"x": {"$gt": MinKey()}}, [i for i in ALL if i != 1]),
        ({"x": {"$gte": MinKey()}}, ALL),
        ({"x": {"$lt": MaxKey()}}, [i for i in ALL if i != 10]),
        ({"x": {"$lte": MaxKey()}}, ALL),
        # ...and the empty directions.
        ({"x": {"$lt": MinKey()}}, []),
        ({"x": {"$lte": MinKey()}}, [1]),
        ({"x": {"$gt": MaxKey()}}, []),
        ({"x": {"$gte": MaxKey()}}, [10]),
    ],
)
def test_a_minkey_or_maxkey_bound_reaches_an_absent_field(coll, query, expected):
    assert _ids(coll, query) == expected


def test_the_absent_document_is_actually_in_the_result(coll):
    """Named separately because it is the whole point: a document with no `x`."""
    assert 3 in _ids(coll, {"x": {"$gt": MinKey()}})
    assert 3 in _ids(coll, {"x": {"$lt": MaxKey()}})


# --- the ordinary bounds must NOT pick it up --------------------------------


# These are the reason the fix is safe: type bracketing already excludes null
# from every bound but MinKey / MaxKey, so comparing an absent field as null
# changes nothing here.
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ({"x": {"$gt": 3}}, [4]),
        ({"x": {"$gte": 5}}, [4]),
        # Measured, not derived: 4 (`x: 5`) and 8 (`x: [1]`, element-wise).
        ({"x": {"$lt": 10}}, [4, 8]),
        ({"x": {"$lte": 5}}, [4, 8]),
        ({"x": {"$gt": "a"}}, [5]),
        ({"x": {"$lt": "z"}}, [5]),
        ({"x": {"$gt": OID}}, []),
        ({"x": {"$gte": OID}}, [12]),
        # null bounds keep their own rule: `$gte` / `$lte` match null AND
        # missing, `$gt` / `$lt` match nothing.
        ({"x": {"$gte": None}}, [2, 3]),
        ({"x": {"$lte": None}}, [2, 3]),
        ({"x": {"$gt": None}}, []),
        ({"x": {"$lt": None}}, []),
    ],
)
def test_an_ordinary_bound_still_excludes_an_absent_field(coll, query, expected):
    assert _ids(coll, query) == expected


# --- equality and membership are unchanged ----------------------------------


@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ({"x": MinKey()}, [1]),
        ({"x": MaxKey()}, [10]),
        ({"x": {"$in": [MinKey()]}}, [1]),
        ({"x": {"$ne": MinKey()}}, [i for i in ALL if i != 1]),
        ({"x": None}, [2, 3]),
        ({"x": {"$exists": False}}, [3]),
    ],
)
def test_equality_and_membership_are_unchanged(coll, query, expected):
    assert _ids(coll, query) == expected
