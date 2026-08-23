"""Sorting on an array-valued field uses one representative element, as mongod does.

mongod sorts an array-valued field by its **minimum** element ascending and its
**maximum** descending. We compared whole arrays instead, which put every array
after every scalar — and had a worse consequence than being merely wrong:

**it disagreed with our own index path.** A multikey index writes one entry per
element, so an IXSCAN already produced mongod's ordering. The same query therefore
returned a different order depending on whether an index happened to exist. An
index must change speed, never results.

Every expectation was measured against a live mongod 6.0.16 on the same documents.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer

# mongod 6.0.16, sorting on `x`:
#   ascending  by minima:  [1,100]=1 < [5,9]=5 < 6 < [7]=7
#   descending by maxima:  [1,100]=100 > [5,9]=9 > [7]=7 > 6
DOCS = [
    {"_id": "a[5,9]", "x": [5, 9]},
    {"_id": "b[1,100]", "x": [1, 100]},
    {"_id": "c[7]", "x": [7]},
    {"_id": "d-scalar-6", "x": 6},
]
ASC = ["b[1,100]", "a[5,9]", "d-scalar-6", "c[7]"]
DESC = ["b[1,100]", "a[5,9]", "c[7]", "d-scalar-6"]


@pytest.fixture
def db(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        try:
            yield client.t
        finally:
            client.close()


def order(coll, direction: int) -> list[str]:
    return [d["_id"] for d in coll.find().sort("x", direction)]


def test_collscan_sorts_arrays_by_min_element(db) -> None:
    db.noidx.insert_many(DOCS)
    assert order(db.noidx, 1) == ASC


def test_collscan_sorts_arrays_by_max_element_descending(db) -> None:
    db.noidx_desc.insert_many(DOCS)
    assert order(db.noidx_desc, -1) == DESC


def test_indexed_and_unindexed_sorts_agree(db) -> None:
    """The bug that made this matter: an index must not change results.

    Both directions now agree across the collection scan and the index scan and
    match mongod. Descending needed a second fix: a multikey index writes a
    whole-array entry alongside the per-element ones, and the backward walk hit
    those first, so first-occurrence dedup picked documents by their whole-array
    key instead of their maximum element.
    """
    db.noidx2.insert_many(DOCS)
    db.idx.insert_many(DOCS)
    db.idx.create_index("x")

    assert order(db.idx, 1) == order(db.noidx2, 1) == ASC
    assert order(db.idx, -1) == order(db.noidx2, -1) == DESC


def test_whole_array_equality_still_uses_the_index(db) -> None:
    """The ordering walk drops whole-array entries; equality must still find them.

    Those entries exist precisely to answer `{x: [5, 9]}`, so dropping them from
    the *ordering* walk must not disturb the lookup path.
    """
    db.eq.insert_many(DOCS)
    db.eq.create_index("x")
    assert [d["_id"] for d in db.eq.find({"x": [5, 9]})] == ["a[5,9]"]
    assert [d["_id"] for d in db.eq.find({"x": 7})] == ["c[7]"]


def test_empty_array_sorts_below_null(db) -> None:
    """mongod places `[]` between MinKey and null — it has no representative."""
    db.empties.insert_many(
        [
            {"_id": "null", "x": None},
            {"_id": "empty", "x": []},
            {"_id": "num", "x": 1},
        ]
    )
    assert order(db.empties, 1) == ["empty", "null", "num"]


def test_array_sorts_relative_to_scalars_by_its_minimum(db) -> None:
    """An array is not simply 'after every scalar'."""
    db.rel.insert_many(
        [
            {"_id": "arr-low", "x": [1, 999]},
            {"_id": "scalar-mid", "x": 500},
            {"_id": "arr-high", "x": [900, 901]},
        ]
    )
    assert order(db.rel, 1) == ["arr-low", "scalar-mid", "arr-high"]


def test_scalars_only_are_unaffected(db) -> None:
    """The common path must not change."""
    db.scalars.insert_many([{"_id": i, "x": v} for i, v in enumerate([3, 1, 2])])
    assert order(db.scalars, 1) == [1, 2, 0]
