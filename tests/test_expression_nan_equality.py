"""NaN in the EXPRESSION language, measured on mongod 8.2.11 (2026-09-09).

Two bugs found by `tools/probes/query_result_sets.py` the first time it was run
with a Python column — the `/tmp` original had compared mongod against the Rust
server only, so the Python side of both was invisible.

The first was a CRASH: `find({"$expr": {"$gt": ["$v", 0]}})` over a collection
holding a `Decimal128("NaN")` answered `internal server error`. The expression
language widens a `Decimal128` to a `decimal.Decimal` before comparing, and
`Decimal("NaN") < 0` raises `decimal.InvalidOperation` — not the `TypeError`
the fallback caught.

The second is the recurring "Python's `==` standing in for a BSON semantic"
shape: `{$eq: [NaN, NaN]}` is TRUE on mongod, and both servers said false
(Rust only for two plain doubles; its Decimal128 branch was already right,
which made the answer depend on which numeric type held the NaN).
"""

from __future__ import annotations

import pymongo
import pytest
from bson import Decimal128

from secantus import SecantusDBServer

NAN = float("nan")
DNAN = Decimal128("NaN")


@pytest.fixture
def coll(tmp_path):
    server = SecantusDBServer(port=0, storage_path=str(tmp_path / "store"))
    server.start()
    host, port = server.address
    client = pymongo.MongoClient(host, port, directConnection=True)
    try:
        yield client["nanexpr"]["c"]
    finally:
        client.close()
        server.stop()


@pytest.mark.parametrize("value", [NAN, DNAN], ids=["double", "decimal128"])
@pytest.mark.parametrize(
    "op,expected",
    [("$gt", False), ("$gte", False), ("$lt", True), ("$lte", True)],
)
def test_a_nan_is_ranked_below_zero_not_a_crash(coll, value, op, expected):
    """NaN sorts below every number, so `$lt` is true and `$gt` is false.

    The `Decimal128` half used to answer `1 internal server error`.
    """
    coll.insert_one({"_id": 1, "v": value})
    rows = list(coll.aggregate([{"$project": {"r": {op: ["$v", 0]}}}]))
    assert rows == [{"_id": 1, "r": expected}]


@pytest.mark.parametrize("value", [NAN, DNAN], ids=["double", "decimal128"])
def test_expr_with_a_nan_in_the_collection_does_not_crash(coll, value):
    """The shape the probe actually hit: a `$expr` filter over a mixed corpus."""
    coll.insert_many([{"_id": 1, "v": value}, {"_id": 2, "v": 5}, {"_id": 3, "v": -5}])
    assert [d["_id"] for d in coll.find({"$expr": {"$gt": ["$v", 0]}})] == [2]
    assert sorted(d["_id"] for d in coll.find({"$expr": {"$lt": ["$v", 0]}})) == [1, 3]


@pytest.mark.parametrize("stored", [NAN, DNAN], ids=["double", "decimal128"])
@pytest.mark.parametrize("arg", [NAN, DNAN], ids=["double", "decimal128"])
def test_nan_equals_nan_in_every_numeric_type_pairing(coll, stored, arg):
    """All four combinations are true on mongod -- the answer must not depend
    on which numeric type happens to hold the NaN."""
    coll.insert_one({"_id": 1, "v": stored})
    rows = list(
        coll.aggregate([{"$project": {"eq": {"$eq": ["$v", arg]}, "ne": {"$ne": ["$v", arg]}}}])
    )
    assert rows == [{"_id": 1, "eq": True, "ne": False}]


def test_nan_still_does_not_equal_a_number(coll):
    coll.insert_one({"_id": 1, "v": NAN})
    rows = list(coll.aggregate([{"$project": {"r": {"$eq": ["$v", 0]}}}]))
    assert rows == [{"_id": 1, "r": False}]


def test_cmp_ranks_a_nan_first(coll):
    coll.insert_one({"_id": 1, "v": DNAN})
    rows = list(coll.aggregate([{"$project": {"r": {"$cmp": ["$v", 0]}}}]))
    assert rows == [{"_id": 1, "r": -1}]


# --- the neighbours the fix must not have moved ---------------------------


def test_a_bool_is_still_not_a_number(coll):
    """`bson_equal`'s original reason for existing."""
    coll.insert_one({"_id": 1, "v": True})
    rows = list(coll.aggregate([{"$project": {"r": {"$eq": ["$v", 1]}}}]))
    assert rows == [{"_id": 1, "r": False}]


def test_signed_zeros_are_still_equal(coll):
    coll.insert_one({"_id": 1, "v": 0.0})
    rows = list(coll.aggregate([{"$project": {"r": {"$eq": ["$v", -0.0]}}}]))
    assert rows == [{"_id": 1, "r": True}]


def test_setting_a_nan_over_the_same_nan_is_not_a_change(coll):
    """Equality and CHANGE DETECTION are different questions, and this fix had
    to leave the second one alone: mongod reports `modifiedCount: 0` here and
    `1` for the signed-zero case below."""
    coll.insert_one({"_id": 1, "a": NAN})
    assert coll.update_one({"_id": 1}, {"$set": {"a": NAN}}).modified_count == 0
    coll.delete_many({})
    coll.insert_one({"_id": 2, "a": 0.0})
    assert coll.update_one({"_id": 2}, {"$set": {"a": -0.0}}).modified_count == 1


def test_a_numeric_type_change_over_a_nan_is_still_a_change(coll):
    """`float` NaN -> `Decimal128` NaN reports `modifiedCount: 1` on mongod."""
    coll.insert_one({"_id": 1, "a": NAN})
    assert coll.update_one({"_id": 1}, {"$set": {"a": DNAN}}).modified_count == 1
