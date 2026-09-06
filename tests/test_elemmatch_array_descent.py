"""`$elemMatch` traverses an array once, not twice.

mongod applies implicit array traversal **once per path step**, and
``$elemMatch`` spends that step choosing the element — so inside it the element
is a terminal value and nothing descends into it again. Both forms got that
wrong, in opposite directions (probed against mongod 8.2.11, 2026-09-06):

* the **operator** form (``{$gt: 1}``) matched *through* an element that was
  itself an array, so ``[[5]]`` and ``[1, [2, [3]]]`` matched
  ``{$elemMatch: {$gt: 1}}``. mongod matches neither.
* the **criteria** form (``{y: 5}``) considered only document elements, so
  ``{$elemMatch: {}}`` — which imposes no field requirement — missed every
  document whose array holds an ARRAY element. mongod returns those.

Both servers had both bugs, identically. The fix is a `descend` flag threaded
through the operator dispatch, off for the element match.

Still divergent and filed in `tasks/backlog.md`, because they live in path
resolution rather than here: a dotted POSITIONAL path (`x.0`) descends one level
too far, and a dotted sort key does not descend at all.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer

# The corpus is the one the rules were derived from; `_id`s are stable so a
# failure names a document.
DOCS = [
    {"_id": 1, "x": 5},
    {"_id": 2, "x": [5]},
    {"_id": 3, "x": [[5]]},
    {"_id": 4, "x": [1, 2, 3]},
    {"_id": 5, "x": [1, [2, [3]]]},
    {"_id": 6, "x": []},
    {"_id": 7, "x": [[]]},
    {"_id": 8, "x": [{"y": 5}]},
    {"_id": 9, "x": [{"y": 5}, {"y": 6}]},
    {"_id": 10, "x": {"y": 5}},
    {"_id": 11, "x": {"y": [5]}},
    {"_id": 12, "x": [{"y": [5]}]},
    {"_id": 13, "x": [[{"y": 5}]]},
    {"_id": 14, "x": {"0": 5}},
    {"_id": 15, "x": [5, 6]},
    {"_id": 16, "x": [[5, 6]]},
]


@pytest.fixture
def coll(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    c = cli["arraydescent"]["c"]
    c.insert_many([dict(d) for d in DOCS])
    try:
        yield c
    finally:
        cli.close()
        srv.stop()


def _ids(coll, q):
    return sorted(d["_id"] for d in coll.find(q))


# --- the operator form must not look inside an array element ----------------


@pytest.mark.parametrize(
    ("condition", "expected"),
    [
        # 3 (`[[5]]`), 5 (`[1,[2,[3]]]`) and 16 (`[[5,6]]`) hold only array
        # elements, and an array is not > 1.
        ({"$gt": 1}, [2, 4, 15]),
        ({"$eq": 5}, [2, 15]),
        # Measured, not derived: an element of `[5]` / `[5, 6]` is not < 3, so
        # only 4 (`[1,2,3]`) and 5 (`[1,[2,[3]]]`, whose `1` qualifies) match.
        ({"$lt": 3}, [4, 5]),
        ({"$in": [5]}, [2, 15]),
        # `$type` sees the element as it is: an array element IS an array.
        ({"$type": "array"}, [3, 5, 7, 13, 16]),
        ({"$type": "object"}, [8, 9, 12]),
        ({"$type": "number"}, [2, 4, 5, 15]),
    ],
)
def test_the_operator_form_treats_the_element_as_terminal(coll, condition, expected):
    assert _ids(coll, {"x": {"$elemMatch": condition}}) == expected


# Operators that recurse or iterate the array THEMSELVES need the flag too, and
# each was a separate miss in the first version of this fix: `$in` has its own
# candidate path, `$not` re-enters the field matcher, and `$all` walks the
# array. Every expectation below is mongod's measured answer.
@pytest.mark.parametrize(
    ("condition", "expected"),
    [
        # Without the flag `$gt: 1` descended into `[5]`, matched, and `$not`
        # inverted it -- so 3 and 16 were wrongly excluded.
        ({"$not": {"$gt": 1}}, [3, 4, 5, 7, 8, 9, 12, 13, 16]),
        ({"$not": {"$eq": 5}}, [3, 4, 5, 7, 8, 9, 12, 13, 15, 16]),
        ({"$not": {"$type": "array"}}, [2, 4, 5, 8, 9, 12, 15]),
        # `$all` walks the field's array; inside `$elemMatch` the element IS the
        # value, so `[[5]]` must not satisfy `$all: [5]`.
        ({"$all": [5]}, [2, 15]),
        # `$size` and a nested `$elemMatch` were already right -- they operate on
        # the element as a whole and never descended.
        ({"$size": 1}, [3, 13]),
        ({"$size": 2}, [5, 16]),
        ({"$elemMatch": {"$gt": 1}}, [3, 5, 16]),
    ],
)
def test_operators_that_recurse_or_iterate_also_stop_at_the_element(coll, condition, expected):
    assert _ids(coll, {"x": {"$elemMatch": condition}}) == expected


# --- the criteria form reaches array elements when it asks nothing of them ---


def test_an_empty_criteria_matches_document_and_array_elements(coll):
    assert _ids(coll, {"x": {"$elemMatch": {}}}) == [3, 5, 7, 8, 9, 12, 13, 16]


@pytest.mark.parametrize(
    ("criteria", "expected"),
    [
        # A non-empty criteria names a field, and an array has none — so 13
        # (`[[{y: 5}]]`) stays out even though the document is in there.
        ({"y": 5}, [8, 9, 12]),
        ({"y": {"$gt": 4}}, [8, 9, 12]),
        ({"y": 99}, []),
    ],
)
def test_a_non_empty_criteria_still_needs_a_document_element(coll, criteria, expected):
    assert _ids(coll, {"x": {"$elemMatch": criteria}}) == expected


# --- neighbouring behaviour that was already right --------------------------


# The one-level descent OUTSIDE `$elemMatch` is unchanged: these are the same
# operators reached by a plain field predicate, where mongod does traverse.
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ({"x": 5}, [1, 2, 15]),
        ({"x": [5]}, [2, 3]),
        ({"x": [[5]]}, [3]),
        ({"x": {"$gt": 4}}, [1, 2, 15]),
        ({"x": {"$type": "array"}}, [2, 3, 4, 5, 6, 7, 8, 9, 12, 13, 15, 16]),
        ({"x": {"$size": 1}}, [2, 3, 7, 8, 12, 13, 16]),
        ({"x.y": 5}, [8, 9, 10, 11, 12]),
    ],
)
def test_the_ordinary_one_level_descent_is_unchanged(coll, query, expected):
    assert _ids(coll, query) == expected


def test_elem_match_on_a_non_array_field_matches_nothing(coll):
    """A scalar field is not a one-element array for `$elemMatch`."""
    assert _ids(coll, {"x": {"$elemMatch": {"$eq": 5}}}) == [2, 15]
    assert 1 not in _ids(coll, {"x": {"$elemMatch": {"$eq": 5}}})
