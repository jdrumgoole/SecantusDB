"""Clustered collections (the ``clusteredIndex`` create option).

mongod makes ``_id`` the clustering key — which is already SecantusDB's
WiredTiger layout (the doc table is keyed by ``_id``), so this is a
metadata + reporting feature: validate at create, echo in
listCollections, and report the clustered index in listIndexes with
``clustered: true`` and no separate ``_id_``. Oracle-pinned against a
real mongod 2026-06-13.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def client(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        mc = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            yield mc
        finally:
            mc.close()


def _ci(name="test index", unique=True, key=None):
    return {"key": key or {"_id": 1}, "unique": unique, "name": name}


def test_create_and_list_collections(client: MongoClient) -> None:
    db = client["cdb"]
    db.create_collection("c", clusteredIndex=_ci())
    opts = next(db.list_collections(filter={"name": "c"}))["options"]
    assert opts["clusteredIndex"] == {
        "v": 2,
        "key": {"_id": 1},
        "name": "test index",
        "unique": True,
    }


def test_list_indexes_reports_clustered(client: MongoClient) -> None:
    db = client["cdb"]
    db.create_collection("c", clusteredIndex=_ci())
    db["c"].insert_many([{"_id": i} for i in range(3)])
    idxs = list(db["c"].list_indexes())
    assert len(idxs) == 1
    assert dict(idxs[0]) == {
        "v": 2,
        "key": {"_id": 1},
        "name": "test index",
        "unique": True,
        "clustered": True,
    }
    # Secondary indexes still appear normally alongside the clustered one.
    db["c"].create_index([("x", 1)])
    names = {ix["name"] for ix in db["c"].list_indexes()}
    assert names == {"test index", "x_1"}


def test_default_name_is_id_underscore(client: MongoClient) -> None:
    db = client["cdb"]
    db.create_collection("c", clusteredIndex={"key": {"_id": 1}, "unique": True})
    opts = next(db.list_collections(filter={"name": "c"}))["options"]
    assert opts["clusteredIndex"]["name"] == "_id_"


def test_rejects_non_id_key(client: MongoClient) -> None:
    with pytest.raises(OperationFailure) as exc:
        client["cdb"].create_collection("c", clusteredIndex={"key": {"x": 1}, "unique": True})
    assert exc.value.code == 197


def test_rejects_non_unique(client: MongoClient) -> None:
    with pytest.raises(OperationFailure) as exc:
        client["cdb"].create_collection("c", clusteredIndex={"key": {"_id": 1}, "unique": False})
    assert exc.value.code == 5979700
