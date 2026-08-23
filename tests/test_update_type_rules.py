"""`$inc` / `$mul` type rules, and `$addToSet`'s field-order-sensitive equality.

Found by a three-way update-operator differential (Python server / Rust server /
live mongod 6.0.16) over 41 cases. Three defects, all on the Python server:

* `$inc` against a string raised a bare `ValueError: invalid literal for int()`
  that escaped as **"internal server error" (code 1)**; mongod answers
  TypeMismatch (14).
* `$inc` against a bool silently computed `n: 2`, because Python makes `bool` a
  subclass of `int`. mongod refuses. This one is worse than the crash: it wrote
  wrong data rather than failing.
* `$mul` had both defects identically — the same `is None`-only check.
* `$addToSet` used `elem not in arr`, i.e. Python `==`, which compares documents
  **order-insensitively**. mongod treats `{y: 2, x: 1}` as a different value from
  `{x: 1, y: 2}` and appends it; we silently dropped it.

Note what is NOT restricted: `$min` / `$max` accept any type and use BSON
cross-type ordering (number < string < bool, null lowest), verified against the
same mongod. Only `$inc` / `$mul` are numeric-only.
"""

from __future__ import annotations

import pytest
from bson import Decimal128, Int64
from pymongo import MongoClient
from pymongo.errors import OperationFailure, WriteError

from secantus import SecantusDBServer

NON_NUMERIC = [
    ("string", "x"),
    ("bool", True),
    ("null", None),
    ("array", [1]),
    ("document", {"a": 1}),
]


@pytest.fixture
def db(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        try:
            yield client.t
        finally:
            client.close()


@pytest.mark.parametrize("op", ["$inc", "$mul"])
@pytest.mark.parametrize("label,value", NON_NUMERIC)
def test_arithmetic_on_non_numeric_is_type_mismatch(db, op, label, value) -> None:
    """mongod answers TypeMismatch (14), never a 500 and never a silent write."""
    coll = db[f"{op[1:]}_{label}"]
    coll.insert_one({"_id": 1, "n": value})

    with pytest.raises((WriteError, OperationFailure)) as exc:
        coll.update_one({"_id": 1}, {op: {"n": 1}})

    assert exc.value.code == 14, "must be TypeMismatch, not InternalError"
    assert coll.find_one({"_id": 1})["n"] == value, "the document must be untouched"


@pytest.mark.parametrize(
    "start,delta,expected",
    [
        (1, 2, 3),
        (Int64(1), 1, Int64(2)),
        (Decimal128("1.5"), 1, Decimal128("2.5")),
        (1, Decimal128("0.5"), Decimal128("1.5")),
    ],
)
def test_inc_still_works_across_the_numeric_lattice(db, start, delta, expected) -> None:
    coll = db.inc_ok
    coll.delete_many({})
    coll.insert_one({"_id": 1, "n": start})
    coll.update_one({"_id": 1}, {"$inc": {"n": delta}})
    got = coll.find_one({"_id": 1})["n"]
    assert got == expected
    assert type(got) is type(expected), "the BSON numeric type must be preserved"


def test_inc_on_a_missing_field_still_treats_it_as_zero(db) -> None:
    db.inc_missing.insert_one({"_id": 1})
    db.inc_missing.update_one({"_id": 1}, {"$inc": {"n": 5}})
    assert db.inc_missing.find_one({"_id": 1})["n"] == 5


def test_mul_on_a_missing_field_still_yields_zero(db) -> None:
    """mongod's rule: multiplying an absent field produces 0, not the factor."""
    db.mul_missing.insert_one({"_id": 1})
    db.mul_missing.update_one({"_id": 1}, {"$mul": {"n": 5}})
    assert db.mul_missing.find_one({"_id": 1})["n"] == 0


def test_addtoset_treats_reordered_documents_as_distinct(db) -> None:
    """mongod's document equality is field-ORDER-sensitive; ours must agree."""
    db.ats.insert_one({"_id": 1, "a": [{"x": 1, "y": 2}]})
    db.ats.update_one({"_id": 1}, {"$addToSet": {"a": {"y": 2, "x": 1}}})
    assert db.ats.find_one({"_id": 1})["a"] == [{"x": 1, "y": 2}, {"y": 2, "x": 1}]


def test_addtoset_still_dedupes_an_identical_document(db) -> None:
    db.ats2.insert_one({"_id": 1, "a": [{"x": 1, "y": 2}]})
    db.ats2.update_one({"_id": 1}, {"$addToSet": {"a": {"x": 1, "y": 2}}})
    assert db.ats2.find_one({"_id": 1})["a"] == [{"x": 1, "y": 2}]


def test_addtoset_scalar_and_each_still_dedupe(db) -> None:
    db.ats3.insert_one({"_id": 1, "a": [1, 2]})
    db.ats3.update_one({"_id": 1}, {"$addToSet": {"a": 1}})
    assert db.ats3.find_one({"_id": 1})["a"] == [1, 2]

    db.ats4.insert_one({"_id": 1, "a": [1]})
    db.ats4.update_one({"_id": 1}, {"$addToSet": {"a": {"$each": [1, 2]}}})
    assert db.ats4.find_one({"_id": 1})["a"] == [1, 2]


def test_addtoset_agrees_with_the_query_matcher(db) -> None:
    """The membership rule and the query rule must be the same rule.

    They were not: the matcher already compared documents order-sensitively while
    `$addToSet` used Python `==`. A reordered document that the matcher says is
    absent must be one `$addToSet` appends.
    """
    db.agree.insert_one({"_id": 1, "a": [{"x": 1, "y": 2}]})
    assert db.agree.count_documents({"a": {"y": 2, "x": 1}}) == 0, "matcher: absent"

    db.agree.update_one({"_id": 1}, {"$addToSet": {"a": {"y": 2, "x": 1}}})
    assert len(db.agree.find_one({"_id": 1})["a"]) == 2, "so it must be appended"


@pytest.mark.parametrize(
    "op,start,operand,expected",
    [
        ("$min", "x", 5, 5),  # number sorts below string
        ("$min", True, 5, 5),  # number sorts below bool
        ("$min", None, 5, None),  # null is lowest
        ("$max", "x", 5, "x"),
        ("$max", True, 5, True),
        ("$max", None, 5, 5),
    ],
)
def test_min_max_are_cross_type_not_numeric_only(db, op, start, operand, expected) -> None:
    """Unlike `$inc`/`$mul`, these accept any type and use BSON sort order."""
    coll = db[f"mm_{op[1:]}_{type(start).__name__}"]
    coll.insert_one({"_id": 1, "n": start})
    coll.update_one({"_id": 1}, {op: {"n": operand}})
    assert coll.find_one({"_id": 1})["n"] == expected
