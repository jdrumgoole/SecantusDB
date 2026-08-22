"""`{x: NaN}` matches a stored NaN, as mongod does.

IEEE says NaN != NaN and Python follows it, so the matcher used to answer "no
match" for a stored NaN. That made a document written with `_id: NaN` **accepted
and then unreachable by its own key** — the write succeeds, the row is in the
collection, and no `_id` query can ever retrieve it again.

The storage layer was never at fault: `sortkey.encode_value` already gives NaN a
stable encoding, so the index entry was correct. Only the equality matcher was
wrong.

Every expectation here was probed against a live mongod 6.0.16, which returns 1
for each of the matching cases below.
"""

from __future__ import annotations

import pytest
from bson.decimal128 import Decimal128
from pymongo import MongoClient

from secantus import SecantusDBServer

NAN = float("nan")
INF = float("inf")


@pytest.fixture
def db(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        try:
            yield client.t
        finally:
            client.close()


def test_nan_field_matches(db) -> None:
    db.a.insert_one({"_id": 1, "x": NAN})
    assert db.a.count_documents({"x": NAN}) == 1


def test_nan_id_is_retrievable_by_its_own_key(db) -> None:
    """The bug's worst form: written, present, and unreachable by `_id`."""
    db.b.insert_one({"_id": NAN, "v": 1})
    assert db.b.count_documents({}) == 1, "sanity: the document was stored"
    assert db.b.count_documents({"_id": NAN}) == 1
    assert db.b.find_one({"_id": NAN})["v"] == 1


def test_decimal128_nan_matches(db) -> None:
    db.d.insert_one({"_id": 1, "x": Decimal128("NaN")})
    assert db.d.count_documents({"x": Decimal128("NaN")}) == 1


def test_nan_matches_across_numeric_types(db) -> None:
    """mongod treats int/double/Decimal128 as one numeric type for equality."""
    db.c.insert_one({"_id": 1, "x": NAN})
    assert db.c.count_documents({"x": Decimal128("NaN")}) == 1


@pytest.mark.parametrize(
    "stored,query",
    [
        (NAN, 1),  # a NaN row must not answer an ordinary numeric query
        (5, NAN),  # a NaN query must not match an ordinary numeric row
        (NAN, INF),  # NaN and infinity are distinct values
        (INF, NAN),
    ],
)
def test_no_false_positives(db, stored, query) -> None:
    db.fp.delete_many({})
    db.fp.insert_one({"_id": 1, "x": stored})
    assert db.fp.count_documents({"x": query}) == 0


def test_infinity_still_matches(db) -> None:
    """Infinity compares fine under IEEE and must be untouched by the NaN path."""
    db.i.insert_one({"_id": 1, "x": INF})
    assert db.i.count_documents({"x": INF}) == 1
    assert db.i.count_documents({"x": -INF}) == 0


def test_nan_does_not_leak_into_ordering(db) -> None:
    """Equality treats NaN as equal to itself; range operators must not.

    mongod sorts NaN below every other number, and `$gt: NaN` matches nothing —
    the NaN-equality rule is confined to equality.
    """
    db.o.insert_many([{"_id": 1, "x": NAN}, {"_id": 2, "x": 5}, {"_id": 3, "x": -1}])
    assert db.o.count_documents({"x": {"$gt": NAN}}) == 0
    assert [d["_id"] for d in db.o.find().sort("x", 1)] == [1, 3, 2]
