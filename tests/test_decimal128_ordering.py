"""Decimal128 takes part in the unified numeric ORDER, on both servers.

mongod treats Decimal128 as one of the numeric types for comparison and sorting,
so a mixed field sorts `Decimal128("1") < 2 < Decimal128("2.5") < 3.0` and
`{$gt: [Decimal128("2.5"), 2]}` is true (probed 8.2.11, 2026-09-02).

The Rust engine's `order::is_sortable` excluded Decimal128 while `order::cmp`
had always handled it -- so every comparison involving a decimal DEFERRED, and a
defer on the standalone server is a generic `BadValue`. That made `$gt`, `$lt`,
`$cmp` and `sort` unusable on a collection holding decimals.

Run against both servers deliberately: the Rust one is where the defect was, and
a Python-only test would have proved nothing about it.
"""

import contextlib

import pytest
from bson import Decimal128

from secantus import SecantusDBServer

DOCS = [
    {"_id": 0, "v": Decimal128("2.5")},
    {"_id": 1, "v": 2},
    {"_id": 2, "v": 3.0},
    {"_id": 3, "v": Decimal128("1")},
    {"_id": 4, "v": "s"},
]


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


@pytest.fixture(params=["python", "rust"])
def seeded(request, tmp_path):
    factory = _python_client if request.param == "python" else _rust_client
    with factory(tmp_path) as client:
        coll = client["dec_order"]["c"]
        coll.insert_many([dict(d) for d in DOCS])
        yield coll


def test_a_mixed_numeric_field_sorts_by_value_not_by_type(seeded):
    """1 < 2 < 2.5 < 3.0, with the string last -- the decimals interleave."""
    assert [d["_id"] for d in seeded.find({}, {"_id": 1}).sort("v", 1)] == [3, 1, 0, 2, 4]


def test_descending_is_the_mirror(seeded):
    assert [d["_id"] for d in seeded.find({}, {"_id": 1}).sort("v", -1)] == [4, 2, 0, 1, 3]


@pytest.mark.parametrize(("op", "expected"), [("$gt", True), ("$lt", False), ("$cmp", 1)])
def test_comparison_expressions_answer(seeded, op, expected):
    got = list(seeded.aggregate([{"$match": {"_id": 0}}, {"$project": {"r": {op: ["$v", 2]}}}]))
    assert got[0]["r"] == expected


def test_a_range_query_finds_the_decimal(seeded):
    """`find({v: {$gt: 2}})` must return the Decimal128 2.5, not skip it."""
    assert sorted(d["_id"] for d in seeded.find({"v": {"$gt": 2}}, {"_id": 1})) == [0, 2]


def test_equality_across_the_numeric_widths(seeded):
    """Decimal128("1") and the int 1 are the same value to mongod."""
    assert sorted(d["_id"] for d in seeded.find({"v": 1}, {"_id": 1})) == [3]
