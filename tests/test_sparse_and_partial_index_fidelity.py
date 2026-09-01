"""An index must never change which documents a query returns.

Every case here was a real divergence from mongod 8.2.11, measured on
2026-09-01 by running the same operations against both servers
(``tools/probes/`` style) and diffing the ``_id`` sets. Three of the four are
silent DATA LOSS -- the query returned fewer documents with the index than
without it, with no error -- which is the failure mode this project treats as
the most serious.

The shape to keep in mind when adding to this file: assert against the SAME
query run on a collection with no index. A result set is only wrong relative to
what the collection actually holds, and the no-index answer is that.
"""

from __future__ import annotations

import pytest

from secantus.storage import Storage


@pytest.fixture()
def storage(tmp_path):
    s = Storage(str(tmp_path / "store"))
    try:
        yield s
    finally:
        s.close()


def _ids(docs):
    return sorted(d["_id"] for d in docs)


def _seed(storage: Storage, docs):
    storage.insert("db", "c", [dict(d) for d in docs])


NULL_DOCS = [
    {"_id": 1, "a": None, "b": 1},
    {"_id": 2, "b": 1},  # `a` ABSENT -- the document a sparse index omits
    {"_id": 3, "a": 5, "b": 1},
    {"_id": 4, "a": [], "b": 1},
]


@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ({"a": None}, [1, 2]),
        ({"a": {"$eq": None}}, [1, 2]),
        ({"a": {"$in": [None, 5]}}, [1, 2, 3]),
        ({"a": None, "b": 1}, [1, 2]),
        # The gate's blind spot on the first pass: a RANGE bound against null
        # matches an absent field too, not just `$eq`.
        ({"a": {"$lte": None}}, [1, 2]),
        ({"a": {"$gte": None}}, [1, 2]),
        # Not affected -- these cannot match an absent field, so the index is
        # complete for them and stays usable.
        ({"a": 5}, [3]),
        ({"a": {"$exists": True}}, [1, 3, 4]),
    ],
)
def test_sparse_index_never_drops_absent_field_documents(storage, query, expected):
    """A sparse index omits documents missing the field; a null-equality query
    MATCHES them. Using the index for such a query lost them outright."""
    _seed(storage, NULL_DOCS)
    storage.create_index("db", "c", "a_sparse", {"a": 1}, {"sparse": True})
    assert _ids(storage.find_matching("db", "c", query)) == expected


def test_sparse_index_is_not_walked_for_a_sort(storage):
    """A sort walks the whole index, so a sparse one silently truncates the
    result set -- every document missing the field vanishes."""
    _seed(storage, NULL_DOCS)
    storage.create_index("db", "c", "a_sparse", {"a": 1}, {"sparse": True})
    assert _ids(storage.find_matching("db", "c", {}, sort={"a": 1})) == [1, 2, 3, 4]


COMPOUND_DOCS = [
    {"_id": 1, "a": 1, "b": 1},
    {"_id": 2, "a": 1},  # has `a` but not `b`
    {"_id": 3, "b": 1},  # has `b` but not `a`
    {"_id": 4, "a": 1, "b": None},
    {"_id": 5},  # neither -- the only document a sparse compound index omits
]


@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ({"a": 1}, [1, 2, 4]),
        ({"a": {"$gte": 0}}, [1, 2, 4]),
        ({"a": 1, "b": None}, [2, 4]),
        ({"a": 1, "b": 1}, [1]),
    ],
)
def test_compound_sparse_index_holds_documents_missing_some_fields(storage, query, expected):
    """mongod's compound-sparse rule is "at least ONE indexed field present";
    requiring ALL of them dropped ``{_id: 2}`` from ``find({a: 1})``."""
    _seed(storage, COMPOUND_DOCS)
    storage.create_index("db", "c", "ab_sparse", {"a": 1, "b": 1}, {"sparse": True})
    assert _ids(storage.find_matching("db", "c", query)) == expected


def test_partial_filter_implication_is_type_bracketed(storage):
    """``{b: "x"}`` does NOT imply ``{b: {$gt: 0}}``.

    A string sorts above every number in BSON order, but the range operators are
    type-bracketed: ``$gt: 0`` matches numbers only. The implication check
    compared with the sort-order encoder, concluded the query was covered, used
    a partial index that does not contain the document, and returned nothing.
    """
    _seed(storage, [{"_id": 1, "a": 5, "b": "x"}, {"_id": 2, "a": 5, "b": 7}])
    storage.create_index("db", "c", "ix", {"a": 1}, {"partialFilterExpression": {"b": {"$gt": 0}}})
    assert _ids(storage.find_matching("db", "c", {"a": 5, "b": "x"})) == [1]
    # Same-bracket implication still holds, so the index is still used here.
    assert _ids(storage.find_matching("db", "c", {"a": 5, "b": 7})) == [2]


def test_query_covered_entirely_by_a_partial_filter_does_not_crash(storage):
    """A query naming only partial-filter fields left no key prefix to pin.

    The lookup built an empty parts list and raised ``IndexError`` out of the
    command handler, which reached the client as an internal error rather than
    an answer.
    """
    _seed(storage, [{"_id": 1, "a": 1, "b": 5}, {"_id": 2, "a": 2, "b": 0}, {"_id": 3, "b": 9}])
    storage.create_index("db", "c", "ix", {"a": 1}, {"partialFilterExpression": {"b": {"$gt": 0}}})
    assert _ids(storage.find_matching("db", "c", {"b": 5})) == [1]
    assert _ids(storage.find_matching("db", "c", {"b": 9})) == [3]
    # `b: 0` does not imply `b > 0`, so this one legitimately falls back.
    assert _ids(storage.find_matching("db", "c", {"b": 0})) == [2]


def test_sparse_index_write_path_keeps_partial_documents(storage):
    """The write-side half of the compound-sparse fix, asserted directly."""
    _seed(storage, COMPOUND_DOCS)
    storage.create_index("db", "c", "ab_sparse", {"a": 1, "b": 1}, {"sparse": True})
    # `{_id: 5}` has neither field and is the only one legitimately absent.
    assert _ids(storage.find_matching("db", "c", {"a": {"$exists": True}})) == [1, 2, 4]
