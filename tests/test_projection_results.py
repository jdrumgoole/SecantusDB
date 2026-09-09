"""What `find`'s `projection` returns, versus what it refuses.

`apply_projection` is on the hot read path of every `find` and had no probe at
all until 2026-09-09; `tools/probes/projection_results.py` found three real
divergences on its first run, all shared by both servers.
"""

from __future__ import annotations

import bson
import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture(scope="module")
def coll(tmp_path_factory):
    path = tmp_path_factory.mktemp("projres")
    server = SecantusDBServer(port=0, storage_path=str(path / "store"))
    server.start()
    host, port = server.address
    client = pymongo.MongoClient(host, port, directConnection=True)
    try:
        yield client["projres"]["c"]
    finally:
        client.close()
        server.stop()


def _one(coll, doc, projection):
    coll.delete_many({})
    coll.insert_one(bson.decode(bson.encode(doc)))
    return coll.find_one({}, projection)


# --- field order ----------------------------------------------------------
#
# mongod emits `_id` first, then the SOURCE DOCUMENT's key order -- not the
# projection spec's -- with computed fields appended. `$slice` and `$elemMatch`
# were applied after the plain inclusions and so landed at the end. Key order is
# what a driver renders, and comparing documents for equality ignores it, so
# nothing else in the suite could see this.

#: Key order `_id, b, a, c` -- deliberately NOT alphabetical, and with `a`
#: after `b`, so a spec-ordered or sorted result is distinguishable.
ORDERED_DOC = {"_id": 1, "b": 9, "a": [1, 2, 3], "c": 7}


@pytest.mark.parametrize(
    "projection,expected",
    [
        ({"a": 1, "b": 1}, ["_id", "b", "a"]),
        # The SPEC's order does not matter -- both give the document's.
        ({"b": 1, "a": 1}, ["_id", "b", "a"]),
        ({"a": {"$slice": 2}, "b": 1}, ["_id", "b", "a"]),
        ({"a": {"$elemMatch": {"$gt": 1}}, "b": 1}, ["_id", "b", "a"]),
        # A computed field is appended after the document's own keys.
        ({"z": {"$literal": 1}, "b": 1}, ["_id", "b", "z"]),
    ],
)
def test_projected_fields_come_back_in_the_documents_order(coll, projection, expected):
    assert list(_one(coll, ORDERED_DOC, projection)) == expected


# --- $elemMatch as an element-VALUE predicate -----------------------------


@pytest.mark.parametrize(
    "array,predicate,expected",
    [
        # A bare operator criterion tests each element AS A VALUE.
        ([1, 2, 3], {"$gt": 2}, [3]),
        ([1, "a"], {"$type": "string"}, ["a"]),
        ([None, 1], {"$eq": None}, [None]),
        # ...with NO implicit one-level array traversal, so a nested array is
        # not descended: `[3, 4]` is not a number.
        ([[1, 2], [3, 4]], {"$gt": 2}, None),
        ([1, [3, 4], 5], {"$gt": 2}, [5]),
        ([[1, 2], [3, 4]], {"$all": [3]}, None),
        # ...but the element is still the array it is, for the operators that
        # care about that. This is the traversal being suppressed, not the type.
        ([[1, 2], [3]], {"$size": 2}, [[1, 2]]),
        ([[1, 2], [3, 4]], {"$eq": [3, 4]}, [[3, 4]]),
    ],
)
def test_a_bare_operator_criterion_is_an_element_value_predicate(coll, array, predicate, expected):
    """This used to raise `2 unknown top level operator: $gt` whenever the
    array held documents, because the branch keyed off the ELEMENT's type
    rather than the criterion's shape."""
    got = _one(coll, {"_id": 1, "a": array}, {"a": {"$elemMatch": predicate}})
    assert got.get("a") == expected


def test_a_document_criterion_is_still_a_per_field_predicate(coll):
    got = _one(
        coll,
        {"_id": 1, "a": [{"x": 1, "y": 9}, {"x": 2, "y": 8}, {"x": 3, "y": 7}]},
        {"a": {"$elemMatch": {"x": {"$gt": 1}}}},
    )
    assert got["a"] == [{"x": 2, "y": 8}]


def test_a_bare_operator_over_documents_omits_the_field(coll):
    got = _one(coll, {"_id": 1, "a": [{"x": 1}, {"x": 2}]}, {"a": {"$elemMatch": {"$gt": 2}}})
    assert "a" not in got


# --- path collision -------------------------------------------------------


@pytest.mark.parametrize(
    "projection,code,message",
    [
        # The ancestor first: names the LATER path and its portion after the
        # first component.
        ({"a": 1, "a.x": 1}, 31249, "Path collision at a.x remaining portion x"),
        ({"a.x": 1, "a.x.y": 1}, 31249, "Path collision at a.x.y remaining portion x.y"),
        # Exclusion collides too, and the check runs ahead of the
        # mix-include-exclude one.
        ({"a": 0, "a.x": 0}, 31249, "Path collision at a.x remaining portion x"),
        ({"a": 0, "a.x": 1}, 31249, "Path collision at a.x remaining portion x"),
        # The descendant first: names the ANCESTOR.
        ({"a.x": 1, "a": 1}, 31250, "Path collision at a"),
    ],
)
def test_an_ancestor_of_another_projected_path_is_refused(coll, projection, code, message):
    """Both servers accepted these and returned a truncated document."""
    coll.delete_many({})
    coll.insert_one({"_id": 1, "a": {"x": 1, "y": 2}, "b": 3})
    with pytest.raises(pymongo.errors.OperationFailure) as excinfo:
        list(coll.find({}, projection))
    assert excinfo.value.code == code
    assert excinfo.value.details["errmsg"] == message


@pytest.mark.parametrize(
    "projection",
    [
        {"a.x": 1, "a.y": 1},  # siblings
        {"a.x": 1, "b": 1},  # unrelated
        {"a": 1, "ab.x": 1},  # a shared string prefix is not a path component
        {"a.x": 1},  # one path
    ],
)
def test_these_are_not_collisions(coll, projection):
    """The rule must not widen: each of these is legal on mongod."""
    coll.delete_many({})
    coll.insert_one({"_id": 1, "a": {"x": 1, "y": 2}, "ab": {"x": 3}, "b": 4})
    assert list(coll.find({}, projection))
