"""``$type: "int"`` / ``"long"`` distinguishes by BSON type tag, not value range.

Before this slice, the `_TYPE_PREDS` table used a Python value-range check
(`-2**31 <= v <= 2**31 - 1`) to distinguish int from long. That meant a doc
inserted as `Int64(5)` (a value that fits in int32 numerically but is
typed as BSON int64) was matched by `$type: "int"` instead of `$type: "long"`.

pymongo's BSON decoder preserves the int32/int64 distinction by class —
int32 round-trips as plain `int`, int64 round-trips as `bson.Int64`. The
fix keys on `isinstance(v, Int64)` instead of value range.

`$convert: {to: "long"}` was also returning a plain `int`, so its result
couldn't be matched by `$type: "long"`. Fixed to return `Int64`.
"""

from __future__ import annotations

import pytest
from bson import Int64
from pymongo import MongoClient

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield c
    finally:
        c.close()


# ---------------------------------------------------------------------------
# $type: "int" / "long" — distinguish by BSON type tag
# ---------------------------------------------------------------------------


def test_int64_in_int32_range_matches_long_not_int(client) -> None:
    """`Int64(5)` fits numerically in int32, but its BSON tag is int64.
    `$type: "long"` must match; `$type: "int"` must not."""
    coll = client["type_db"]["int64_small"]
    coll.insert_one({"_id": 1, "x": Int64(5)})

    docs = list(coll.find({"x": {"$type": "long"}}))
    assert [d["_id"] for d in docs] == [1]

    docs = list(coll.find({"x": {"$type": "int"}}))
    assert docs == []


def test_plain_int_matches_int_not_long(client) -> None:
    """A plain Python int(5) inserted via pymongo encodes as int32 → BSON
    int32 → decodes as plain int. `$type: "int"` matches; `$type: "long"` doesn't."""
    coll = client["type_db"]["plain_int"]
    coll.insert_one({"_id": 1, "x": 5})

    docs = list(coll.find({"x": {"$type": "int"}}))
    assert [d["_id"] for d in docs] == [1]

    docs = list(coll.find({"x": {"$type": "long"}}))
    assert docs == []


def test_large_int_encodes_as_int64(client) -> None:
    """A Python int that doesn't fit in int32 is BSON-encoded as int64
    by the driver. So `2**40` round-trips as `Int64` and `$type: "long"`
    matches it (and `$type: "int"` does not)."""
    coll = client["type_db"]["large_int"]
    coll.insert_one({"_id": 1, "x": 2**40})

    docs = list(coll.find({"x": {"$type": "long"}}))
    assert [d["_id"] for d in docs] == [1]

    docs = list(coll.find({"x": {"$type": "int"}}))
    assert docs == []


def test_type_number_matches_both_int_and_long(client) -> None:
    """`$type: "number"` is the umbrella — int32, int64, double, decimal
    all qualify (bool does not)."""
    coll = client["type_db"]["number_umbrella"]
    coll.insert_many(
        [
            {"_id": 1, "x": 5},  # int32
            {"_id": 2, "x": Int64(5)},  # int64
            {"_id": 3, "x": 5.5},  # double
            {"_id": 4, "x": True},  # bool — excluded
            {"_id": 5, "x": "5"},  # string — excluded
        ]
    )
    docs = sorted(coll.find({"x": {"$type": "number"}}), key=lambda d: d["_id"])
    assert [d["_id"] for d in docs] == [1, 2, 3]


def test_type_numeric_code_form(client) -> None:
    """Numeric codes (16 = int32, 18 = int64) and string aliases agree."""
    coll = client["type_db"]["numeric_code"]
    coll.insert_many(
        [
            {"_id": 1, "x": 5},
            {"_id": 2, "x": Int64(5)},
        ]
    )
    # Code 16 = "int"
    assert [d["_id"] for d in coll.find({"x": {"$type": 16}})] == [1]
    # Code 18 = "long"
    assert [d["_id"] for d in coll.find({"x": {"$type": 18}})] == [2]


def test_type_array_form_matches_either(client) -> None:
    """`$type: ["int", "long"]` matches a value tagged as either."""
    coll = client["type_db"]["array_form"]
    coll.insert_many(
        [
            {"_id": 1, "x": 5},
            {"_id": 2, "x": Int64(5)},
            {"_id": 3, "x": "five"},
        ]
    )
    docs = sorted(coll.find({"x": {"$type": ["int", "long"]}}), key=lambda d: d["_id"])
    assert [d["_id"] for d in docs] == [1, 2]


# ---------------------------------------------------------------------------
# $convert: {to: "long"} returns Int64, not plain int
# ---------------------------------------------------------------------------


def test_convert_to_long_returns_int64(client) -> None:
    """`$convert: {input: x, to: "long"}` produces a value that matches
    `$type: "long"` on a subsequent `$match` stage."""
    coll = client["type_db"]["convert_long"]
    coll.insert_many(
        [
            {"_id": 1, "n": 5},  # int32
            {"_id": 2, "n": "10"},  # string
        ]
    )
    pipeline = [
        {"$addFields": {"as_long": {"$convert": {"input": "$n", "to": "long"}}}},
        {"$match": {"as_long": {"$type": "long"}}},
    ]
    docs = sorted(coll.aggregate(pipeline), key=lambda d: d["_id"])
    assert [d["_id"] for d in docs] == [1, 2]
    # And the int32 stays int32 — original `n` doesn't change.
    assert isinstance(docs[0]["n"], int) and not isinstance(docs[0]["n"], Int64)


def test_convert_to_int_stays_int32(client) -> None:
    """`$convert: {to: "int"}` produces a plain int (int32), distinct from long."""
    coll = client["type_db"]["convert_int"]
    coll.insert_one({"_id": 1, "n": Int64(7)})

    pipeline = [
        {"$addFields": {"as_int": {"$convert": {"input": "$n", "to": "int"}}}},
        {"$project": {"as_int_is_int": {"$eq": [{"$type": "$as_int"}, "int"]}}},
    ]
    # Easier: just match on $type.
    pipeline = [
        {"$addFields": {"as_int": {"$convert": {"input": "$n", "to": "int"}}}},
        {"$match": {"as_int": {"$type": "int"}}},
    ]
    assert len(list(coll.aggregate(pipeline))) == 1
