"""Gap tests for comparing a double with a Decimal128 or a large Int64
(section 7.080 of `tasks/backlog-rust-mongodb.md`). mongod compares numbers
of different types by their exact values; the Rust server compares a double
by its shortest decimal representation, so `0.1` equals `Decimal128("0.1")`
and `2**62` as a double is not the Int64 `2**62`. Every answer is mongod
8.2.11's, measured 2026-10-15 on a one-member replica set, with and without
an index on the field (they agree).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

pymongo = pytest.importorskip("pymongo")
bson = pytest.importorskip("bson")

BIG = float(2**62)


def _seeded(client: Any, name: str, indexed: bool) -> Any:
    from bson.decimal128 import Decimal128
    from bson.int64 import Int64

    client.drop_database(name)
    coll = client[name].t
    coll.insert_many(
        [
            {"_id": 1, "a": Decimal128("0.1")},
            {"_id": 2, "a": 0.1},
            {"_id": 3, "a": Decimal128("0.1000000000000000055511151231257827")},
            {"_id": 4, "a": Int64(2**62)},
            {"_id": 5, "a": BIG},
            {"_id": 6, "a": Int64(4611686018427387950)},
        ]
    )
    if indexed:
        coll.create_index("a")
    return coll


# mongod 8.2.11's answers, the same with and without the index.
EXPECTED = [
    ({"a": 0.1}, [2]),
    ({"a": {"$ne": 0.1}}, [1, 3, 4, 5, 6]),
    ({"a": {"$lt": 0.1}}, [1, 3]),
    ({"a": {"$lte": 0.1}}, [1, 2, 3]),
    ({"a": {"$gt": 0.1}}, [4, 5, 6]),
    ({"a": BIG}, [4, 5]),
    ({"a": {"$gt": BIG}}, [6]),
    ({"a": {"$gte": BIG}}, [4, 5, 6]),
    ({"a": {"$lt": BIG}}, [1, 2, 3]),
    ({"a": {"$ne": BIG}}, [1, 2, 3, 6]),
]


@pytest.mark.backlog_fixed("RM-152")
@pytest.mark.parametrize("indexed", [False, True], ids=["collscan", "index"])
def test_double_compared_with_decimal_and_int64_exactly(rust_mongo: Any, indexed: bool) -> None:
    """mongod 8.2.11: a double equals a Decimal128 or an Int64 only when
    their exact values are equal, and orders by exact value."""
    coll = _seeded(rust_mongo, "rmnum_q", indexed)
    got = [
        (q, mongo_answer(lambda q=q: sorted(d["_id"] for d in coll.find(q)))) for q, _ in EXPECTED
    ]
    assert got == [(q, ("ok", ids)) for q, ids in EXPECTED]


@pytest.mark.backlog_fixed("RM-152")
def test_id_uniqueness_compares_numbers_exactly(rust_mongo: Any) -> None:
    """mongod 8.2.11: `_id: 2**62` as a double duplicates the Int64, and
    `_id: 0.1` does not duplicate `Decimal128("0.1")`."""
    from bson.decimal128 import Decimal128
    from bson.int64 import Int64

    rust_mongo.drop_database("rmnum_id")
    coll = rust_mongo.rmnum_id.t
    got = []
    for value in (Int64(2**62), BIG, Decimal128("0.1"), 0.1):
        reply = mongo_answer(lambda value=value: coll.insert_one({"_id": value}).inserted_id)
        got.append(reply[0] if reply[0] == "ok" else reply[1])
    assert got == ["ok", 11000, "ok", "ok"]
