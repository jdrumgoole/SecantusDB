"""Range operators are TYPE-BRACKETED, and a JavaScript value is not a string.

``{v: {$gt: 3}}`` matches numbers greater than 3 and nothing else -- not the
string ``"z"``, not a date, not a ``MaxKey``. Measured against mongod 8.2.11 on
2026-09-01, 96 of 112 (bound, operator, collation) shapes disagreed: only three
brackets (bool, document, array) were enforced, so a collection holding a
``MaxKey`` returned that document for *every* ``$gt`` query -- pymongo's
``MaxKey.__gt__`` returns True unconditionally, which is what made it look like
a match.

The JavaScript half is the same root cause seen from the other side:
``bson.Code`` SUBCLASSES ``str``, so it took the string type rank, compared as a
collated string, and -- because ``Code`` is unhashable and ``sort_levels`` is
``lru_cache``d -- crashed an ordinary collated sort with
``1 internal server error``.
"""

from __future__ import annotations

import datetime

import pytest
from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer

#: One document per BSON type bracket, so a bound of any type has exactly one
#: in-bracket neighbour and thirteen out-of-bracket ones.
CORPUS = [
    {"_id": 1, "v": Code("x=1")},
    {"_id": 2, "v": "abc"},
    {"_id": 3, "v": 5},
    {"_id": 4, "v": Binary(b"ab")},
    {"_id": 5, "v": Regex("a", "i")},
    {"_id": 6, "v": Timestamp(1, 1)},
    {"_id": 7, "v": MinKey()},
    {"_id": 8, "v": MaxKey()},
    {"_id": 9, "v": Decimal128("2.5")},
    {"_id": 10, "v": datetime.datetime(2020, 1, 1)},
    {"_id": 11, "v": ObjectId("507f1f77bcf86cd799439011")},
    {"_id": 12, "v": True},
    {"_id": 13, "v": None},
    {"_id": 14, "v": [1, 2]},
    {"_id": 15, "v": {"a": 1}},
    {"_id": 16, "v": Int64(7)},
]


@pytest.fixture
def coll(wt_home):
    with SecantusDBServer(port=0, storage_path=wt_home) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            things = client["testdb"]["things"]
            things.insert_many(CORPUS)
            yield things
        finally:
            client.close()


def _ids(coll, query, collation=None):
    cursor = coll.find(query, {"_id": 1})
    if collation:
        cursor = cursor.collation(collation)
    return sorted(d["_id"] for d in cursor)


@pytest.mark.parametrize("collation", [None, {"locale": "en", "strength": 2}])
@pytest.mark.parametrize(
    "bound,expected_gt",
    [
        # A numeric bound sees only the numbers: 5, Decimal128(2.5), Int64(7).
        (3, [3, 16]),
        # A string bound sees only the string -- NOT the JavaScript value.
        ("ab", [2]),
        (Binary(b"a"), [4]),
        (Timestamp(0, 1), [6]),
        (Code("a"), [1]),
        (datetime.datetime(2019, 1, 1), [10]),
        (ObjectId("000000000000000000000000"), [11]),
        # `[1, 2]` is here because a range operator on an array field matches
        # per ELEMENT, and 2 > 1. mongod agrees; bracketing does not change it.
        (Decimal128("1"), [3, 9, 14, 16]),
        (True, []),
    ],
)
def test_gt_is_bracketed_to_the_bounds_type(coll, bound, expected_gt, collation):
    assert _ids(coll, {"v": {"$gt": bound}}, collation) == expected_gt


@pytest.mark.parametrize("collation", [None, {"locale": "en", "strength": 2}])
def test_minkey_and_maxkey_bounds_compare_across_every_type(coll, collation):
    # The exception to bracketing, and it is the BOUND that gets it: a document
    # whose value is a MaxKey is still bracketed out of `{$gt: 3}` above.
    every_id = [d["_id"] for d in CORPUS]
    assert _ids(coll, {"v": {"$gt": MinKey()}}, collation) == [i for i in every_id if i != 7]
    assert _ids(coll, {"v": {"$gte": MinKey()}}, collation) == every_id
    assert _ids(coll, {"v": {"$lt": MaxKey()}}, collation) == [i for i in every_id if i != 8]
    assert _ids(coll, {"v": {"$lte": MaxKey()}}, collation) == every_id


def test_maxkey_document_does_not_match_every_range_query(coll):
    # The headline regression: this returned [3, 8, 16].
    assert 8 not in _ids(coll, {"v": {"$gt": 3}})
    assert 7 not in _ids(coll, {"v": {"$lt": 3}})


@pytest.mark.parametrize("op", ["$gt", "$gte", "$lt", "$lte"])
def test_regex_bound_is_rejected_not_silently_empty(coll, op):
    # mongod refuses this at parse time; answering an empty result set hid a
    # malformed query behind "nothing matched".
    with pytest.raises(OperationFailure) as exc:
        _ids(coll, {"v": {op: Regex("a", "")}})
    assert exc.value.code == 2
    assert "Can't have RegEx as arg to non-equality predicate over field 'v'." in str(exc.value)


def test_ne_with_a_regex_bound_is_rejected(coll):
    with pytest.raises(OperationFailure) as exc:
        _ids(coll, {"v": {"$ne": Regex("a", "")}})
    assert exc.value.code == 2
    assert "Can't have regex as arg to $ne." in str(exc.value)


def test_javascript_sorts_between_regex_and_maxkey(coll):
    """mongod's cross-type sort order, with and without an index.

    `bson.Code` subclasses `str`, so an `isinstance(value, str)` test catches
    one -- and BOTH rank tables had such a test, so every JavaScript value
    sorted among the strings. The two have to move together:
    `ordering._bson_type_rank` drives the in-memory sort while
    `sortkey.encode_value` writes the rank byte persisted index entries are
    sorted by, so changing one alone makes an index change the sort answer.
    They moved together, and `entryFormat` went to 3 so a store written before
    it is refused rather than read back in the old order.
    """
    ids = {"_id": {"$in": [1, 2, 5, 6, 8]}}
    expected = [2, 6, 5, 1, 8]  # string < Timestamp < Regex < JavaScript < MaxKey
    assert [d["_id"] for d in coll.find(ids).sort("v", 1)] == expected
    coll.create_index([("v", 1)])
    assert [d["_id"] for d in coll.find(ids).sort("v", 1)] == expected, (
        "an index changed the sort answer"
    )


def test_javascript_matching_is_bracketed_even_with_an_index(coll):
    # MATCHING is bracketed correctly either way -- the exact match pass
    # rechecks every index candidate, so this half needs no format change.
    coll.create_index([("v", 1)])
    assert _ids(coll, {"v": {"$gt": "ab"}}) == [2]
    assert _ids(coll, {"v": {"$gt": Code("a")}}) == [1]


def test_collated_sort_over_a_javascript_value_does_not_crash(coll):
    """`sort_levels` is `lru_cache`d and `Code` is unhashable, so a collated
    sort over a collection holding one answered `1 internal server error`.

    """
    ids = {"_id": {"$in": [1, 2, 5]}}
    collated = [
        d["_id"] for d in coll.find(ids).sort("v", 1).collation({"locale": "en", "strength": 2})
    ]
    # A collation has nothing to say about JavaScript, so the two orders agree.
    assert collated == [d["_id"] for d in coll.find(ids).sort("v", 1)]
    assert collated == [2, 5, 1]
