"""A dotted sort key walks one array level, as mongod's does.

`sort({"x.y": 1})` over array-of-subdocument data came back in the wrong order:
mongod ranks `x: [{y: 1}]` among the documents that HAVE an `x.y` — by 1, its
representative element — and both servers ranked it with the documents that
have none. Wrong order is wrong RESULTS as soon as a `limit` is involved.

The sort was resolving the path with `get_path`, which deliberately does not
walk through an array (that is what `$set` and projection need). The
array-descending resolver `get_path_values` already existed for INDEX key
generation and already has mongod's semantics, including stopping at one level:
`x: [[{y: 5}]]` has no `x.y` on mongod either. Using it for the sort also makes
the in-memory order agree with the index path by construction — an index must
change speed, never results, and that is asserted below.

Probed against mongod 8.2.11 (2026-09-06).

Still divergent and filed: a dotted POSITIONAL component (`x.0`) is ambiguous —
mongod tries it as an array index AND as a literal field name, descends only for
the second, and raises `16746 Ambiguous field name found in array` when a SORT
hits both readings. That is a separate piece of work in path resolution.
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer

DOCS = [
    {"_id": 1, "x": [5]},
    {"_id": 2, "x": [[5]]},
    {"_id": 5, "x": [5, 6]},
    {"_id": 6, "x": [[5, 6]]},
    {"_id": 8, "x": [{"y": 5}]},
    {"_id": 9, "x": [[{"y": 5}]]},
    {"_id": 10, "x": [1, [2, [3]]]},
    {"_id": 12, "x": 5},
    {"_id": 15, "x": {"y": 5}},
    {"_id": 16, "x": {"y": [5]}},
    {"_id": 17, "x": [{"y": [5]}]},
]


@pytest.fixture
def coll(wt_home):
    srv = SecantusDBServer(port=0, storage_path=wt_home)
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    c = cli["dottedsort"]["c"]
    c.insert_many([dict(d) for d in DOCS])
    try:
        yield c
    finally:
        cli.close()
        srv.stop()


def _order(coll, spec, hint=None):
    cur = coll.find({}).sort([*spec, ("_id", 1)])
    if hint:
        cur = cur.hint(hint)
    return [d["_id"] for d in cur]


def test_a_dotted_sort_key_descends_one_array_level(coll):
    """8 (`[{y: 5}]`) and 17 (`[{y: [5]}]`) HAVE an `x.y`; 9 (`[[{y: 5}]]`) does
    not, because that is two levels."""
    assert _order(coll, [("x.y", 1)]) == [1, 2, 5, 6, 9, 10, 12, 8, 15, 16, 17]


def test_the_same_descent_descending(coll):
    assert _order(coll, [("x.y", -1)]) == [8, 15, 16, 17, 1, 2, 5, 6, 9, 10, 12]


def test_several_values_sort_by_the_representative_element(coll):
    """A path through an array can yield several values; ascending takes the
    minimum and descending the maximum, the same rule one array value follows."""
    coll.delete_many({})
    coll.insert_many(
        [
            {"_id": 1, "x": [{"y": 5}, {"y": 6}]},
            {"_id": 2, "x": [{"y": 1}]},
            {"_id": 3, "x": [{"y": 9}]},
        ]
    )
    assert _order(coll, [("x.y", 1)]) == [2, 1, 3], "by minima 1 < 5 < 9"
    assert _order(coll, [("x.y", -1)]) == [3, 1, 2], "by maxima 9 > 6 > 1"


# --- neighbouring behaviour that must not move ------------------------------


def test_an_undotted_sort_is_unchanged(coll):
    """The regression this fix nearly shipped: applying the representative-element
    rule twice put `x: [[5]]` among the NUMBERS instead of the arrays."""
    assert _order(coll, [("x", 1)]) == [10, 1, 5, 12, 8, 15, 16, 17, 2, 6, 9]


def test_an_empty_array_still_sorts_between_minkey_and_null(coll):
    coll.delete_many({})
    coll.insert_many([{"_id": 1, "x": []}, {"_id": 2, "x": None}, {"_id": 3, "x": 5}])
    assert _order(coll, [("x", 1)]) == [1, 2, 3]
    assert _order(coll, [("x", -1)]) == [3, 2, 1]


def test_a_missing_path_still_ranks_with_null(coll):
    coll.delete_many({})
    coll.insert_many([{"_id": 1}, {"_id": 2, "x": {"y": None}}, {"_id": 3, "x": {"y": 5}}])
    assert _order(coll, [("x.y", 1)]) == [1, 2, 3]


@pytest.mark.parametrize("direction", [1, -1])
def test_an_index_does_not_change_the_order(coll, direction):
    """The whole reason to use the index layer's own resolver: the in-memory
    sort and an index walk must agree."""
    without = _order(coll, [("x.y", direction)])
    coll.create_index([("x.y", direction)], name="xy")
    assert _order(coll, [("x.y", direction)]) == without
    assert _order(coll, [("x.y", direction)], hint="xy") == without
