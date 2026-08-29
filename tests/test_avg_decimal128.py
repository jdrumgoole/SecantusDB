"""`$avg` over Decimal128 — it used to crash the server.

A group mixing Decimal128 with any other numeric type threw
``TypeError: unsupported operand type(s) for +=: 'float' and 'Decimal128'``
out of ``_acc_avg``, which reached the client as a bare "internal server error".
``$sum`` had always used the type-preserving ``bson_add``; ``$avg`` was missed and
used a raw ``+=``.

Fixing the crash exposed a second bug underneath it: the quotient came back with
27 significant digits where mongod gives 34, because Python's default decimal
context is 28 while Decimal128 carries 34.

Both expectations were probed against a live mongod 6.0.16 on the same documents.
"""

from __future__ import annotations

import pytest
from bson import Decimal128, Int64
from pymongo import MongoClient

from secantus import SecantusDBServer

AVG = [{"$group": {"_id": None, "a": {"$avg": "$x"}}}]


@pytest.fixture
def db(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        try:
            yield client.t
        finally:
            client.close()


def avg_of(coll, docs):
    coll.insert_many(docs)
    return list(coll.aggregate(AVG))[0]["a"]


def test_mixed_numeric_types_do_not_crash(db) -> None:
    """The original crash: Decimal128 alongside int / long / double / bool."""
    got = avg_of(
        db.mixed,
        [
            {"x": 0},
            {"x": -0.0},
            {"x": 5},
            {"x": Int64(5)},
            {"x": Decimal128("5.000")},
            {"x": 5.0},
            {"x": True},  # bool is not numeric for accumulators
            {"x": None},
            {},
        ],
    )
    # mongod 6.0.16 on these exact documents.
    assert str(got) == "3.333333333333333333333333333333333"


def test_decimal128_average_keeps_full_precision(db) -> None:
    """34 significant digits, not Python's default context of 28."""
    got = avg_of(
        db.thirds, [{"x": Decimal128("1")}, {"x": Decimal128("1")}, {"x": Decimal128("1")}]
    )
    assert isinstance(got, Decimal128)
    assert str(got) == "1"

    got = avg_of(db.recurring, [{"x": Decimal128("10")}, {"x": Decimal128("0")}])
    assert str(got) == "5"


def test_decimal128_result_stays_decimal(db) -> None:
    """An all-Decimal128 group must not narrow to float."""
    got = avg_of(db.dec, [{"x": Decimal128("1")}, {"x": Decimal128("2")}])
    assert isinstance(got, Decimal128)
    assert str(got) == "1.5"


def test_plain_numbers_are_unaffected(db) -> None:
    """The common path still answers a float, as before."""
    got = avg_of(db.ints, [{"x": 1}, {"x": 2}])
    assert got == 1.5
    assert not isinstance(got, Decimal128)


def test_all_non_numeric_group_averages_to_null(db) -> None:
    """mongod yields null rather than dropping the field or erroring."""
    assert avg_of(db.strs, [{"x": "nope"}, {"x": None}, {}]) is None


def test_single_decimal_value(db) -> None:
    got = avg_of(db.one, [{"x": Decimal128("7.25")}])
    assert isinstance(got, Decimal128)
    assert str(got) == "7.25"
