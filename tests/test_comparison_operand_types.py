"""Comparison operators over every BSON type, on the RUST server.

`$cmp` / `$gt` / `$gte` / `$lt` / `$lte` / `$eq` / `$ne` — and `$expr`, which
routes through them — were gated on `order::is_sortable`. That predicate guards
the SORT engines, where a type lacking a transitive same-type arm would corrupt
an ordering, and it deliberately excludes NaN, Binary, Timestamp, Regex,
JavaScript and Min/MaxKey.

A single comparison needs no transitivity, and mongod compares every BSON type
by its canonical rank. Gating on the narrow predicate turned each of those into
`2 ... not supported by the Rust server`, so **one `BinData` document made an
entire `$expr` query fail**.

Measured against mongod 8.2.11 on 2026-09-08 over 19 value classes × 3 operands
× 7 operators: **120 of 399 cells diverged**, now 0.

The Python server was already correct here — its `_bson_lt` covers the wider
set — so this is a Rust-only gap, found by sweeping the Rust server against
mongod rather than against the other engine.
"""

from __future__ import annotations

import datetime

import pytest
from bson import Binary, Code, Decimal128, MaxKey, MinKey, ObjectId, Regex, Timestamp

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

OID = ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")
WHEN = datetime.datetime(2026, 1, 2, 3, 4, 5)


@pytest.fixture(scope="module")
def coll(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("cmpops") / "wt"), 0)
    host, port = srv.address
    client = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    c = client["cmpops"]["c"]
    c.insert_one({"_id": 1})
    try:
        yield c
    finally:
        client.close()
        srv.stop()


def compute(coll, expr):
    return list(coll.aggregate([{"$addFields": {"x": expr}}]))[0]["x"]


def lit(v):
    return {"$literal": v}


#: mongod's canonical BSON order, ascending. `$cmp` against a value of each
#: class is the compact way to pin it.
CMP_AGAINST_ZERO = [
    ("MinKey", MinKey(), -1),
    ("null", None, -1),
    ("NaN", float("nan"), -1),
    ("Decimal NaN", Decimal128("NaN"), -1),
    ("int", 1, 1),
    ("double", 1.5, 1),
    ("Decimal", Decimal128("1.5"), 1),
    ("string", "abc", 1),
    ("document", {"k": 1}, 1),
    ("array", [1, 2], 1),
    ("Binary", Binary(b"z", 0), 1),
    ("ObjectId", OID, 1),
    ("bool false", False, 1),
    ("bool true", True, 1),
    ("date", WHEN, 1),
    ("Timestamp", Timestamp(1, 1), 1),
    ("Regex", Regex("a", "i"), 1),
    ("Code", Code("x=1"), 1),
    ("MaxKey", MaxKey(), 1),
]


@pytest.mark.parametrize(
    "label,value,expected", CMP_AGAINST_ZERO, ids=[c[0] for c in CMP_AGAINST_ZERO]
)
def test_cmp_against_a_number_follows_the_canonical_rank(coll, label, value, expected):
    assert compute(coll, {"$cmp": [lit(value), lit(0)]}) == expected


@pytest.mark.parametrize(
    "label,value,expected", CMP_AGAINST_ZERO, ids=[c[0] for c in CMP_AGAINST_ZERO]
)
def test_gt_agrees_with_cmp(coll, label, value, expected):
    assert compute(coll, {"$gt": [lit(value), lit(0)]}) is (expected > 0)


def test_nan_sorts_below_every_other_number(coll):
    """Not `equal`, which is what Python's `<`-is-false-both-ways gives, and
    which `order::cmp` returned until this was measured."""
    assert compute(coll, {"$cmp": [lit(float("nan")), lit(5)]}) == -1
    assert compute(coll, {"$cmp": [lit(5), lit(float("nan"))]}) == 1
    assert compute(coll, {"$cmp": [lit(float("nan")), lit(float("nan"))]}) == 0
    assert compute(coll, {"$cmp": [lit(float("nan")), lit(float("-inf"))]}) == -1


@pytest.mark.parametrize(
    "value,above_zero",
    [
        (Binary(b"z", 0), True),
        (Timestamp(1, 1), True),
        (Regex("a", "i"), True),
        (Code("x=1"), True),
        (MaxKey(), True),
        # MinKey ranks BELOW every number -- the one in this set that does.
        (MinKey(), False),
    ],
    ids=["Binary", "Timestamp", "Regex", "Code", "MaxKey", "MinKey"],
)
def test_one_exotic_document_does_not_break_an_expr_query(coll, value, above_zero):
    """The shape that motivated this: a `$expr` filter over a collection
    holding one of these answered `2 ... not supported` for the whole query."""
    db = coll.database
    scratch = db["exprscan"]
    scratch.delete_many({})
    scratch.insert_many([{"_id": "n", "v": 1}, {"_id": "x", "v": value}])
    got = sorted(d["_id"] for d in scratch.find({"$expr": {"$gt": ["$v", 0]}}, {"_id": 1}))
    assert got == (["n", "x"] if above_zero else ["n"])


def test_expr_over_a_nan_document(coll):
    db = coll.database
    scratch = db["exprnan"]
    scratch.delete_many({})
    scratch.insert_many([{"_id": "n", "v": 1}, {"_id": "nan", "v": float("nan")}])
    got = sorted(d["_id"] for d in scratch.find({"$expr": {"$gt": ["$v", 0]}}, {"_id": 1}))
    assert got == ["n"]  # NaN ranks below 0
