"""Update operators over awkward value classes, compared on the RESULT.

`tools/probes/update_operators.py` compares update ERRORS. Nothing compared the
**document a successful update produces**, which is where a silently wrong write
hides. A sweep of 31 updates × 17 seed value classes (527 cells) against mongod
8.2.11 on 2026-09-08 found **30 divergent** in four families, two of which
corrupted data rather than merely missing an error. All 527 now agree.

The root cause of most of it is one shape CLAUDE.md already names: **missing
conflated with null**. `get_path(doc, path, default=None)` returns `None` for a
field that is absent *and* for one that is present and null, so `$push`,
`$addToSet` and `$bit` all treated a null field as an absence and created a
value over it.
"""

from __future__ import annotations

import pytest
from bson import Regex
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture(scope="module")
def coll(tmp_path_factory):
    home = tmp_path_factory.mktemp("upwrite") / "wt"
    with SecantusDBServer(port=0, storage_path=str(home)) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        c = client["upwrite"]["c"]
        try:
            yield c
        finally:
            client.close()


def apply(coll, seed, update):
    coll.delete_many({})
    coll.insert_one({"_id": 1, **seed})
    coll.update_one({"_id": 1}, update)
    got = coll.find_one({"_id": 1})
    got.pop("_id")
    return got


def error(coll, seed, update):
    coll.delete_many({})
    coll.insert_one({"_id": 1, **seed})
    with pytest.raises(OperationFailure) as excinfo:
        coll.update_one({"_id": 1}, update)
    err = excinfo.value
    return err.code, err.details.get("errmsg", "").split(":: caused by :: ")[-1]


# ---------------------------------------------------------------------------
# A present NULL is not an absent field
# ---------------------------------------------------------------------------

ARRAY_OPS_ON_NULL = [
    (
        {"$push": {"v": 4}},
        2,
        "The field 'v' must be an array but is of type null in document {_id: 1}",
    ),
    (
        {"$push": {"v": {"$each": [4, 5]}}},
        2,
        "The field 'v' must be an array but is of type null in document {_id: 1}",
    ),
    (
        {"$addToSet": {"v": 4}},
        2,
        "Cannot apply $addToSet to non-array field. Field named 'v' has non-array type null",
    ),
]


@pytest.mark.parametrize("update,code,message", ARRAY_OPS_ON_NULL, ids=lambda v: str(v)[:30])
def test_array_operators_refuse_a_null_field(coll, update, code, message):
    """These used to WRITE `{v: [4]}`, destroying the null."""
    assert error(coll, {"v": None}, update) == (code, message)


@pytest.mark.parametrize("update", [{"$push": {"v": 4}}, {"$addToSet": {"v": 4}}])
def test_array_operators_still_create_an_absent_field(coll, update):
    """The other half of the distinction: absent really is created."""
    assert apply(coll, {"w": 1}, update) == {"w": 1, "v": [4]}


def test_bit_refuses_a_null_field(coll):
    """`get_path(..., default=0) or 0` turned every FALSY present value into the
    integer 0, so a null passed the integral check and was overwritten."""
    code, message = error(coll, {"v": None}, {"$bit": {"v": {"and": 1}}})
    assert code == 2
    assert "Cannot apply $bit to a value of non-integral type" in message
    assert "of non-integer type null" in message


@pytest.mark.parametrize(
    "value,type_name", [(-0.0, "double"), ([], "array"), (1.5, "double"), ("a", "string")]
)
def test_bit_refuses_every_non_integral_type(coll, value, type_name):
    code, message = error(coll, {"v": value}, {"$bit": {"v": {"and": 1}}})
    assert code == 2
    assert f"of non-integer type {type_name}" in message


def test_bit_still_starts_an_absent_field_at_zero(coll):
    assert apply(coll, {"w": 1}, {"$bit": {"v": {"or": 4}}}) == {"w": 1, "v": 4}


@pytest.mark.parametrize("value,type_name", [(None, "null"), (1, "int")])
def test_pop_refuses_a_non_array(coll, value, type_name):
    code, message = error(coll, {"v": value}, {"$pop": {"v": 1}})
    assert (code, message) == (14, f"Path 'v' contains an element of non-array type '{type_name}'")


# ---------------------------------------------------------------------------
# $pull: a SCALAR criterion is exact equality; an operator or regex traverses
# ---------------------------------------------------------------------------

PULL = [
    # The bug: `1` is inside the element, but the element is not `1`.
    ([[1, 2]], 1, [[1, 2]]),
    ([[1], [2]], 1, [[1], [2]]),
    ([["a"], "a"], "a", [["a"]]),
    ([[None]], None, [[None]]),
    ([[{"k": 1}]], {"k": 1}, [[{"k": 1}]]),
    # Whole-element equality still removes it.
    ([[1, 2]], [1, 2], []),
    ([1, 2, 3], 1, [2, 3]),
    ([None, 1], None, [1]),
    # An OPERATOR criterion traverses, so both elements go.
    ([[1, 2], [3]], {"$gt": 1}, []),
    ([1, 2, 3], {"$gt": 1}, [1]),
    ([[1, 2]], {"$size": 2}, []),
    # A sub-document criterion matches document elements only.
    ([{"k": 1}, {"k": 2}], {"k": 1}, [{"k": 2}]),
]


@pytest.mark.parametrize("seed,criterion,expected", PULL, ids=lambda v: str(v)[:24])
def test_pull_scalar_is_exact_equality(coll, seed, criterion, expected):
    assert apply(coll, {"v": seed}, {"$pull": {"v": criterion}}) == {"v": expected}


def test_pull_regex_traverses(coll):
    """A regex criterion behaves like the operator form, not the scalar one."""
    assert apply(coll, {"v": [["a"], "a"]}, {"$pull": {"v": Regex("a")}}) == {"v": []}


# ---------------------------------------------------------------------------
# $rename through a non-document path is an error, not a no-op
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "value,rendered",
    [(1, "1"), ([1, 2], "[ 1, 2 ]"), (None, "null"), ("abc", '"abc"'), (True, "true")],
    ids=["int", "array", "null", "string", "bool"],
)
def test_rename_through_a_non_document_errors(coll, value, rendered):
    code, message = error(coll, {"v": value}, {"$rename": {"v.k": "v.j"}})
    assert code == 28
    assert message == f"cannot use the part (v of v.k) to traverse the element ({{v: {rendered}}})"


def test_rename_through_a_document_still_works(coll):
    assert apply(coll, {"v": {"k": 1}}, {"$rename": {"v.k": "v.j"}}) == {"v": {"j": 1}}


def test_rename_of_an_absent_path_is_a_no_op(coll):
    """Absent is not the same as blocked -- it stays a silent no-op."""
    assert apply(coll, {"w": 1}, {"$rename": {"v.k": "v.j"}}) == {"w": 1}
