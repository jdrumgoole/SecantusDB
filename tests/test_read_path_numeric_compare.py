"""Read-path fidelity: numeric comparison, NaN ordering, and `$type` aliases.

From a 385-case differential sweep of `find` filters, projections and sorts
against mongod 8.2.11 (2026-09-06). Every expectation here is that server's
answer, and every one of these was a **wrong result set** — documents missing
from a query that should return them, or ordered wrongly — not an error-message
difference.

Four root causes:

1. **A NaN range bound matched nothing.** mongod's comparison order treats NaN
   as EQUAL to NaN (which is why `find({x: NaN})` works), so an INCLUSIVE bound
   matches it: `{$gte: NaN}` and `{$lte: NaN}` return the NaN document, while
   `{$gt: NaN}` and `{$lt: NaN}` return nothing. IEEE says every NaN comparison
   is false, so both servers returned nothing for all four.
2. **`_coerce_numeric` bailed on any non-finite operand.** It guarded with
   `is_finite()`, which excludes the infinities as well as NaN — and only NaN
   needs excluding. `Decimal` orders ±Infinity perfectly well, so the bail-out
   left `float > Decimal128` to raise `TypeError`, which the caller swallows
   into a silent no-match.
3. **`$all` compared elements with a bare `==`.** mongod treats int / long /
   double / Decimal128 as one type for equality, so `{$all: [5]}` matches a
   document holding `Decimal128("5")`; `==` does not.
4. **NaN had no place in the sort order.** `_bson_lt` answered False in both
   directions, so the sort left a NaN wherever the algorithm put it — between
   `5.5` and `Infinity` in a measured case. mongod places it below every other
   number, `-Infinity` included.

Plus BinData ordering, which is by LENGTH first and then bytes — the Rust server
already had this right and the Python one compared lexicographically.
"""

from __future__ import annotations

import math

import pymongo
import pytest
from bson import Binary, Decimal128
from bson.int64 import Int64

from secantus import SecantusDBServer

NAN, INF = float("nan"), float("inf")


@pytest.fixture
def coll(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli["readpath"]["c"]
    finally:
        cli.close()
        srv.stop()


def _seed(coll, values):
    coll.drop()
    coll.insert_many([{"_id": i, "x": v} for i, v in enumerate(values)])


# --- 1. a NaN range bound ---------------------------------------------------

_NAN_CORPUS = [0, 5, 5.5, NAN, INF, -INF, "abc", None]


@pytest.mark.parametrize(
    ("op", "expected_ids"),
    [
        ("$gte", [3]),  # the NaN document, and only it
        ("$lte", [3]),
        ("$gt", []),  # a strict bound matches nothing
        ("$lt", []),
    ],
)
def test_a_nan_range_bound_matches_only_nan(coll, op, expected_ids):
    _seed(coll, _NAN_CORPUS)
    got = sorted(d["_id"] for d in coll.find({"x": {op: NAN}}))
    assert got == expected_ids


def test_a_nan_bound_does_not_disturb_ordinary_bounds(coll):
    """The NaN document must not leak into a numeric range that excludes it."""
    _seed(coll, _NAN_CORPUS)
    assert sorted(d["_id"] for d in coll.find({"x": {"$gte": 5}})) == [1, 2, 4]
    assert sorted(d["_id"] for d in coll.find({"x": {"$lte": 5}})) == [0, 1, 5]
    # Equality still reaches it, as it always did.
    assert sorted(d["_id"] for d in coll.find({"x": NAN})) == [3]


def test_a_decimal128_nan_bound_behaves_the_same(coll):
    _seed(coll, _NAN_CORPUS)
    dnan = Decimal128("NaN")
    assert sorted(d["_id"] for d in coll.find({"x": {"$gte": dnan}})) == [3]
    assert sorted(d["_id"] for d in coll.find({"x": {"$gt": dnan}})) == []


# --- 2. Decimal128 against the infinities -----------------------------------

_DEC_CORPUS = [0, 1, 5, Int64(5), 5.0, Decimal128("5"), 5.5, INF, -INF]


@pytest.mark.parametrize(
    ("spec", "expected_ids"),
    [
        ({"$gt": Decimal128("5")}, [6, 7]),  # 5.5 and +Infinity
        ({"$gte": Decimal128("5")}, [2, 3, 4, 5, 6, 7]),
        ({"$lt": Decimal128("5")}, [0, 1, 8]),  # 0, 1 and -Infinity
        ({"$lte": Decimal128("5")}, [0, 1, 2, 3, 4, 5, 8]),
        ({"$lt": INF}, [0, 1, 2, 3, 4, 5, 6, 8]),  # includes the Decimal128
        ({"$gt": -INF}, [0, 1, 2, 3, 4, 5, 6, 7]),
        ({"$gte": INF}, [7]),
        ({"$lte": -INF}, [8]),
    ],
)
def test_decimal128_compares_against_the_infinities(coll, spec, expected_ids):
    _seed(coll, _DEC_CORPUS)
    assert sorted(d["_id"] for d in coll.find({"x": spec})) == expected_ids


# --- 3. `$all` bridges the numeric types ------------------------------------


def test_all_matches_across_the_numeric_types(coll):
    _seed(coll, [5, Int64(5), 5.0, Decimal128("5"), [5], "5", 6])
    assert sorted(d["_id"] for d in coll.find({"x": {"$all": [5]}})) == [0, 1, 2, 3, 4]
    # bool stays a separate type, as it does for `$eq`.
    _seed(coll, [1, True])
    assert sorted(d["_id"] for d in coll.find({"x": {"$all": [1]}})) == [0]


# --- 4. NaN's place in the sort order ---------------------------------------


def test_nan_sorts_below_every_other_number(coll):
    _seed(coll, [5.5, NAN, INF, -INF, 0, None])
    got = [d["_id"] for d in coll.find({}).sort([("x", 1), ("_id", 1)])]
    # null, then NaN, then -Infinity .. +Infinity.
    assert got == [5, 1, 3, 4, 0, 2]
    got_desc = [d["_id"] for d in coll.find({}).sort([("x", -1), ("_id", 1)])]
    assert got_desc == list(reversed([5, 1, 3, 4, 0, 2]))


def test_two_nans_tie_rather_than_ordering_arbitrarily(coll):
    _seed(coll, [NAN, 1, NAN])
    got = [d["_id"] for d in coll.find({}).sort([("x", 1), ("_id", 1)])]
    assert got == [0, 2, 1]


# --- BinData orders by length, then bytes -----------------------------------


def test_bindata_sorts_by_length_then_bytes(coll):
    _seed(coll, [Binary(b""), Binary(b"\x02"), Binary(b"\x01\x02"), Binary(b"\x01")])
    got = [d["_id"] for d in coll.find({}).sort([("x", 1), ("_id", 1)])]
    assert got == [0, 3, 1, 2], "b'' < b'\\x01' < b'\\x02' < b'\\x01\\x02'"


# --- `$type` reaches every BSON type ----------------------------------------


def test_type_aliases_that_the_rust_table_omitted(coll):
    """`$type` accepted these aliases and then matched nothing."""
    from bson import Code, MaxKey, MinKey, Timestamp

    _seed(coll, [Code("f"), MinKey(), MaxKey(), Timestamp(1, 1), 5, "s"])
    for alias, expected in (
        ("javascript", [0]),
        ("minKey", [1]),
        ("maxKey", [2]),
        ("timestamp", [3]),
        ("int", [4]),
        ("string", [5]),
    ):
        got = sorted(d["_id"] for d in coll.find({"x": {"$type": alias}}))
        assert got == expected, f"$type {alias}"


def test_type_numeric_codes_match_their_aliases(coll):
    from bson import Code, MaxKey, MinKey, Timestamp

    _seed(coll, [Code("f"), MinKey(), MaxKey(), Timestamp(1, 1)])
    for code, expected in ((13, [0]), (-1, [1]), (127, [2]), (17, [3])):
        got = sorted(d["_id"] for d in coll.find({"x": {"$type": code}}))
        assert got == expected, f"$type {code}"


def test_nan_is_a_double_and_a_number(coll):
    _seed(coll, [NAN, 5, "s"])
    assert sorted(d["_id"] for d in coll.find({"x": {"$type": "double"}})) == [0]
    assert sorted(d["_id"] for d in coll.find({"x": {"$type": "number"}})) == [0, 1]


def test_math_isnan_sanity():
    """Guards the fixture itself: a NaN that is not a NaN proves nothing."""
    assert math.isnan(NAN)
