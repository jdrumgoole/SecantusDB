"""NaN's two orders, on both servers.

mongod uses NaN two different ways and both are correct:

* the RANGE operators exclude it entirely -- `{$lt: 0}` does not match NaN, and
  neither does `{$gt: -Infinity}` -- while `{$eq: NaN}` does, because NaN *is*
  equal to itself for equality;
* the SORT order places it below `-Infinity`, and `$min` / `$max` follow the
  sort order rather than the comparison one.

Mixing the two produced a silent-data-loss bug and a wrong write, one of each,
on both engines. Neither was reachable from the probe corpora until they were
widened on 2026-09-03 -- no probe here contained a NaN.
"""

from __future__ import annotations

import contextlib

import pytest

from secantus import SecantusDBServer

NAN = float("nan")
INF = float("inf")


@contextlib.contextmanager
def _python_client(tmp_path):
    import pymongo

    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        client = pymongo.MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        try:
            yield client
        finally:
            client.close()


@contextlib.contextmanager
def _rust_client(tmp_path):
    import pymongo

    _server = pytest.importorskip("_secantus_server")
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        host, port = srv.address
        client = pymongo.MongoClient(
            host, port, directConnection=True, serverSelectionTimeoutMS=5000
        )
        try:
            yield client
        finally:
            client.close()
    finally:
        srv.stop()


@pytest.fixture(scope="module", params=["python", "rust"])
def _client(request, tmp_path_factory):
    factory = _python_client if request.param == "python" else _rust_client
    with factory(tmp_path_factory.mktemp("nanorder")) as client:
        yield client


@pytest.fixture
def db(_client):
    d = _client["nanorder"]
    for name in ("c", "bare"):
        d.drop_collection(name)
    return d


def test_a_partial_index_does_not_swallow_a_nan_row(db):
    """SILENT DATA LOSS. `find({b: NaN})` returned NOTHING when a partial index
    on `{b: {$lte: 1.5}}` existed, and the row when it did not.

    NaN satisfies no range comparison, so it is not IN that index -- but the
    implication check compared sort KEYS, where NaN orders below every number,
    and concluded the query implied the filter. The type-bracket gate added for
    the previous instance of this bug does not catch it: NaN is inside the
    numeric bracket.
    """
    docs = [{"_id": 1, "b": NAN}, {"_id": 2, "b": 1}, {"_id": 3, "b": 5}]
    db.c.insert_many([dict(d) for d in docs])
    db.bare.insert_many([dict(d) for d in docs])
    db.c.create_index([("a", 1)], name="ix", partialFilterExpression={"b": {"$lte": 1.5}})

    indexed = sorted(d["_id"] for d in db.c.find({"b": NAN}))
    unindexed = sorted(d["_id"] for d in db.bare.find({"b": NAN}))
    assert indexed == unindexed == [1]


def test_the_range_operators_exclude_nan(db):
    """Including `$gte: -Infinity`, which bounds every other number."""
    db.c.insert_many(
        [{"_id": 1, "b": NAN}, {"_id": 2, "b": -INF}, {"_id": 3, "b": 0}, {"_id": 4, "b": INF}]
    )
    assert sorted(d["_id"] for d in db.c.find({"b": {"$lt": 0}})) == [2]
    assert sorted(d["_id"] for d in db.c.find({"b": {"$gte": -INF}})) == [2, 3, 4]
    # ...but equality matches it.
    assert sorted(d["_id"] for d in db.c.find({"b": NAN})) == [1]


def test_the_sort_order_places_nan_below_negative_infinity(db):
    db.c.insert_many(
        [{"_id": 1, "b": NAN}, {"_id": 2, "b": -INF}, {"_id": 3, "b": 0}, {"_id": 4, "b": INF}]
    )
    assert [d["_id"] for d in db.c.find(sort=[("b", 1)])] == [1, 2, 3, 4]


@pytest.mark.parametrize(
    ("seed", "update", "expected_nan"),
    [
        # $min follows the SORT order, in which NaN is the smallest number --
        # so it SETS the field. Both engines used IEEE comparison, where every
        # NaN comparison is false, and left the document untouched.
        ({"a": 5}, {"$min": {"a": NAN}}, True),
        ({"a": -INF}, {"$min": {"a": NAN}}, True),
        # ...and declines in the other direction.
        ({"a": NAN}, {"$min": {"a": 5}}, True),
        ({"a": NAN}, {"$max": {"a": 5}}, False),
        ({"a": 5}, {"$max": {"a": NAN}}, False),
    ],
)
def test_min_and_max_use_the_sort_order_not_the_comparison_one(db, seed, update, expected_nan):
    db.c.insert_one({"_id": 1, **seed})
    db.c.update_one({"_id": 1}, update)
    got = db.c.find_one({"_id": 1})["a"]
    assert (got != got) is expected_nan, f"{seed} {update} -> {got!r}"


def test_min_and_max_still_order_ordinary_values(db):
    """A guard against fixing NaN by breaking the common case."""
    db.c.insert_one({"_id": 1, "a": 5})
    db.c.update_one({"_id": 1}, {"$min": {"a": 3}})
    assert db.c.find_one({"_id": 1})["a"] == 3
    db.c.update_one({"_id": 1}, {"$max": {"a": 9}})
    assert db.c.find_one({"_id": 1})["a"] == 9
    # Cross-type still orders by BSON rank rather than raising.
    db.c.update_one({"_id": 1}, {"$min": {"a": "s"}})
    assert db.c.find_one({"_id": 1})["a"] == 9
