"""`bson.Code` as a stored VALUE, across the operations that touch one.

`Code` subclasses `str` AND defines `__eq__` without `__hash__`. That pair has
produced seven bugs across this and two earlier batches — a crash wherever an
unhashable value reaches a set / dict / `lru_cache`, and a wrong answer wherever
an `isinstance(v, str)` catches one. Every previous sweep fed `Code` in as an
ARGUMENT; this file covers it as stored DATA, which is the other side.
"""

from __future__ import annotations

import pytest
from bson import Code
from pymongo import MongoClient

from secantus import SecantusDBServer

CORPUS = [
    {"_id": 1, "v": Code("x=1")},
    {"_id": 2, "v": "x=1"},  # equal TEXT, different BSON type
    {"_id": 3, "v": 5},
    {"_id": 4, "v": Code("x=1", {"a": 1})},  # javascriptWithScope
    {"_id": 5, "v": Code("a=2")},
]


@pytest.fixture
def coll(wt_home):
    with SecantusDBServer(port=0, storage_path=wt_home) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            things = client["testdb"]["things"]
            things.insert_many([dict(d) for d in CORPUS])
            yield things
        finally:
            client.close()


def test_group_by_a_javascript_value_does_not_crash(coll):
    """`_hashable` passed scalars through, so an unhashable `Code` reached
    `key not in groups` and answered `1 internal server error`."""
    ids = [d["_id"] for d in coll.aggregate([{"$group": {"_id": "$v"}}])]
    assert len(ids) == 5, ids


def test_group_keeps_javascript_and_an_equal_string_apart(coll):
    """mongod buckets by BSON value, so `Code("x=1")` and `"x=1"` are two
    groups. A `str(value)` surrogate would have merged them."""
    keys = [d["_id"] for d in coll.aggregate([{"$group": {"_id": "$v"}}])]
    assert Code("x=1") in keys and "x=1" in keys


def test_collated_group_over_a_javascript_value(coll):
    """The collation path folds strings; `Code` must not go through it."""
    keys = [
        d["_id"]
        for d in coll.aggregate(
            [{"$group": {"_id": "$v"}}], collation={"locale": "en", "strength": 2}
        )
    ]
    assert len(keys) == 5


@pytest.mark.parametrize(
    "alias,expected",
    [
        # These four aliases VALIDATED but had no predicate, so the query
        # silently matched nothing -- four BSON types were unreachable.
        ("javascript", [1, 5]),
        ("javascriptWithScope", [4]),
        # A `Code` is NOT a string, however much it subclasses one.
        ("string", [2]),
        ("int", [3]),
    ],
)
def test_type_alias_reaches_every_bson_type(coll, alias, expected):
    assert sorted(d["_id"] for d in coll.find({"v": {"$type": alias}}, {"_id": 1})) == expected


def test_type_numeric_codes_agree_with_the_aliases(coll):
    for code, alias in ((13, "javascript"), (15, "javascriptWithScope"), (2, "string")):
        by_code = sorted(d["_id"] for d in coll.find({"v": {"$type": code}}, {"_id": 1}))
        by_alias = sorted(d["_id"] for d in coll.find({"v": {"$type": alias}}, {"_id": 1}))
        assert by_code == by_alias, alias
