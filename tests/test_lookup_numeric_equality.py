"""``$lookup`` joins by BSON equality on both servers, not by representation.

A ``localField`` / ``foreignField`` join whose foreign field has no index runs
as a hash join. It keyed on the raw value: Python's ``Decimal128`` hashes and
compares by representation, and the Rust server compared with the derived
structural ``Bson ==``. So ``Decimal128("1.5")`` missed ``Decimal128("1.500")``,
``2`` missed ``Decimal128("2.0")`` (and, on Rust, ``2.0``), NaN missed NaN,
and Python joined ``True`` to ``1``. No error, just missing or extra rows. The
indexed path goes through the query engine and was always right.

Expected results measured on mongod 8.2.11 (2026-09-19); the differential
gate carries the same shape as ``lookup-numeric-equality-by-value``.
"""

from __future__ import annotations

import pytest
from bson import Decimal128, Int64

pymongo = pytest.importorskip("pymongo")

LOCAL = [
    {"_id": 1, "v": Decimal128("1.5")},
    {"_id": 2, "v": 2},
    {"_id": 3, "v": 2.0},
    {"_id": 4, "v": [Int64(7)]},
    {"_id": 5, "v": float("nan")},
    {"_id": 6, "v": True},
]
FOREIGN = [
    {"_id": 10, "v": Decimal128("1.500")},
    {"_id": 11, "v": Decimal128("2.0")},
    {"_id": 12, "v": [7.0, 8]},
    {"_id": 13, "v": Decimal128("NaN")},
    {"_id": 14, "v": 1},
]
EXPECTED = [(1, [10]), (2, [11]), (3, [11]), (4, [12]), (5, [13]), (6, [])]


def _python_server(tmp_path):
    from secantus import SecantusDBServer

    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "py"))
    srv.start()
    return srv, srv.address


def _rust_server(tmp_path):
    rs = pytest.importorskip("_secantus_server")
    srv = rs.RustServer(str(tmp_path / "rs" / "wt"), 0)
    return srv, srv.address


@pytest.mark.parametrize("make", [_python_server, _rust_server], ids=["python", "rust"])
@pytest.mark.parametrize("indexed", [False, True], ids=["hash-join", "indexed"])
def test_lookup_joins_numerics_by_value(tmp_path, make, indexed: bool) -> None:
    srv, (host, port) = make(tmp_path)
    client = pymongo.MongoClient(host, port, directConnection=True)
    try:
        db = client.lk
        db.c.insert_many(LOCAL)
        db.stock.insert_many(FOREIGN)
        if indexed:
            db.stock.create_index("v")
        out = db.c.aggregate(
            [
                {"$lookup": {"from": "stock", "localField": "v", "foreignField": "v", "as": "s"}},
                {"$sort": {"_id": 1}},
            ]
        )
        assert [(d["_id"], sorted(m["_id"] for m in d["s"])) for d in out] == EXPECTED
    finally:
        client.close()
        srv.stop()
