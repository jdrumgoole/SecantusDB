"""Errors the Rust server used to lose or answer as a value, in the pipeline.

Four defects, all measured against mongod 8.2.11 on 2026-09-08:

1. **`$group` and friends DISCARDED every named error.** `group.rs`, `fill.rs`,
   `densify.rs` and `windowfields.rs` typed their errors as `Result<T, ()>`, so
   an error the expression engine had named was thrown away at the module
   boundary and the client got the generic "not supported by the Rust server" --
   for errors reported correctly through every other stage.
2. **`$sortByCount` rejected every expression.** An unconditional early match arm
   answered 40147 for ANY document argument, shadowing a later arm seventy lines
   below that already implemented mongod's rules correctly.
3. **`$arrayElemAt` answered `null` for a non-numeric index.** Only `bool` was
   checked, so `{$arrayElemAt: [[1, 2], "x"]}` was a silent WRONG VALUE. mongod
   errors 28690 naming the type. A decimal index was also rejected, and a whole
   number outside int32 came back as a missing field instead of 28691.
4. **`$divide` / `$mod` by zero deferred**, each with a comment citing what
   "Python raises" -- and a defer has no Python behind it here, so dividing by
   zero blamed the operator rather than the operand.

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import datetime

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

from bson import Binary, Decimal128, ObjectId, Regex  # noqa: E402


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_pipeerr") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["pipeerr"]
    d.c.drop()
    d.c.insert_one({"_id": 1, "n": 5})
    try:
        yield d
    finally:
        cli.close()


# --- 1. named errors survive the accumulator stages ------------------------

NAMED_IN_GROUP = [
    ({"$ln": 0}, 28766),
    ({"$log": [0, 2]}, 28758),
    ({"$trim": {}}, 50695),
    ({"$zip": {}}, 34465),
    ({"$map": {}}, 16880),
    ({"$reduce": {}}, 40077),
]


@pytest.mark.parametrize(
    "expr,code", NAMED_IN_GROUP, ids=[next(iter(e)) for e, _ in NAMED_IN_GROUP]
)
def test_group_reports_the_named_error(db, expr, code):
    """`$group` used to answer `2 BadValue ... not supported by the Rust server`."""
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(db.c.aggregate([{"$group": {"_id": None, "x": {"$first": expr}}}]))
    assert e.value.code == code


@pytest.mark.parametrize(
    "expr,code", NAMED_IN_GROUP, ids=[next(iter(e)) for e, _ in NAMED_IN_GROUP]
)
def test_bucket_reports_the_named_error(db, expr, code):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(
            db.c.aggregate(
                [
                    {
                        "$bucket": {
                            "groupBy": "$n",
                            "boundaries": [0, 10],
                            "output": {"x": {"$first": expr}},
                        }
                    }
                ]
            )
        )
    assert e.value.code == code


def test_a_valid_group_still_works(db):
    assert list(db.c.aggregate([{"$group": {"_id": None, "n": {"$sum": 1}}}])) == [
        {"_id": None, "n": 1}
    ]


# --- 2. $sortByCount takes an expression ----------------------------------


def test_sort_by_count_accepts_an_expression(db):
    """The shadowing arm rejected every document argument with 40147."""
    assert list(db.c.aggregate([{"$sortByCount": {"$add": ["$n", 1]}}])) == [{"_id": 6, "count": 1}]


def test_sort_by_count_accepts_a_path(db):
    assert list(db.c.aggregate([{"$sortByCount": "$n"}])) == [{"_id": 5, "count": 1}]


def test_sort_by_count_still_rejects_an_empty_object(db):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(db.c.aggregate([{"$sortByCount": {}}]))
    assert e.value.code == 40147


def test_sort_by_count_rejects_a_literal_document(db):
    """A document is an EXPRESSION only when its FIRST key is `$`-prefixed.

    `{a: 1}` and `{a: {$add: [...]}}` are literal documents and get 40147, the
    same as `{}`. Removing the shadowing arm alone let these fall through to the
    engine, which deferred -- the 740-shape stage-spec probe caught it.
    """
    for spec in ({"a": 1}, {"a": {"$add": ["$n", 1]}}):
        with pytest.raises(pymongo.errors.OperationFailure) as e:
            list(db.c.aggregate([{"$sortByCount": spec}]))
        assert e.value.code == 40147, spec


def test_sort_by_count_accepts_a_literal_expression(db):
    assert list(db.c.aggregate([{"$sortByCount": {"$literal": 7}}])) == [{"_id": 7, "count": 1}]


def test_sort_by_count_still_rejects_a_number(db):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(db.c.aggregate([{"$sortByCount": 5}]))
    assert e.value.code == 40149


# --- 3. $arrayElemAt index types ------------------------------------------

# (label, index, expected) where expected is ("ok", value) or ("err", code).
# Measured across 19 shapes; `null` and a missing field really are null, every
# numeric works, and everything else is 28690 naming the type.
INDEX_CASES = [
    ("null", None, ("ok", None)),
    ("int", 1, ("ok", 20)),
    ("double", 1.0, ("ok", 20)),
    ("decimal", Decimal128("1"), ("ok", 20)),
    ("string", "x", ("err", 28690)),
    ("bool", True, ("err", 28690)),
    ("array", [1], ("err", 28690)),
    ("doc", {"a": 1}, ("err", 28690)),
    ("date", datetime.datetime(2020, 1, 1), ("err", 28690)),
    ("oid", ObjectId(), ("err", 28690)),
    ("binary", Binary(b"z"), ("err", 28690)),
    ("regex", Regex("a"), ("err", 28690)),
    ("double-frac", 1.5, ("err", 28691)),
    ("decimal-frac", Decimal128("1.5"), ("err", 28691)),
    ("double-huge", 1e40, ("err", 28691)),
    ("decimal-huge", Decimal128("1e40"), ("err", 28691)),
]


@pytest.mark.parametrize("label,index,expected", INDEX_CASES, ids=[c[0] for c in INDEX_CASES])
def test_array_elem_at_index_types(db, label, index, expected):
    pipe = [{"$addFields": {"x": {"$arrayElemAt": [[10, 20], index]}}}]
    kind, want = expected
    if kind == "ok":
        assert list(db.c.aggregate(pipe))[0].get("x") == want
    else:
        with pytest.raises(pymongo.errors.OperationFailure) as e:
            list(db.c.aggregate(pipe))
        assert e.value.code == want


def test_array_elem_at_names_the_type(db):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(db.c.aggregate([{"$addFields": {"x": {"$arrayElemAt": [[10, 20], "x"]}}}]))
    assert "must be a numeric value, but is string" in str(e.value)


def test_an_out_of_range_index_is_still_a_missing_field(db):
    """In-bounds type, out-of-bounds value: mongod omits the field, no error."""
    got = list(db.c.aggregate([{"$addFields": {"x": {"$arrayElemAt": [[10, 20], 9]}}}]))[0]
    assert "x" not in got


# --- 4. divide / mod by zero ----------------------------------------------


def test_divide_by_zero_is_named(db):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(db.c.aggregate([{"$addFields": {"x": {"$divide": [1, 0]}}}]))
    assert e.value.code == 2
    assert "can't $divide by zero" in str(e.value)


def test_mod_by_zero_is_named(db):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(db.c.aggregate([{"$addFields": {"x": {"$mod": [1, 0]}}}]))
    assert e.value.code == 16610
    assert "can't $mod by zero" in str(e.value)


def test_ordinary_division_still_works(db):
    assert list(db.c.aggregate([{"$addFields": {"x": {"$divide": [10, 4]}}}]))[0]["x"] == 2.5
    assert list(db.c.aggregate([{"$addFields": {"x": {"$mod": [-5, 2]}}}]))[0]["x"] == -1
