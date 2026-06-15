"""``admin.system.version`` is a synthetic read-only view that
surfaces the auth-schema version doc.

Some user-management tools (and a handful of driver tests) read
``{_id: "authSchema"}`` from ``admin.system.version`` on startup
to gate which user-management features they offer. Before this
slice the namespace was empty; now it returns the canonical
mongod-shaped doc:

    {_id: "authSchema", currentVersion: 5}

Currentversion 5 is the SCRAM-SHA-256 baseline (MongoDB 4.0+),
which is what SecantusDB implements.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield c
    finally:
        c.close()


# ---------------------------------------------------------------------------
# Read path
# ---------------------------------------------------------------------------


def test_find_returns_authschema_doc(client) -> None:
    """The canonical doc — ``{_id: "authSchema", currentVersion: 5}`` —
    surfaces on a bare find()."""
    docs = list(client["admin"]["system.version"].find())
    assert docs == [{"_id": "authSchema", "currentVersion": 5}]


def test_find_one_by_id_authschema(client) -> None:
    """Lookup by ``_id: "authSchema"`` — the actual pattern tools use."""
    doc = client["admin"]["system.version"].find_one({"_id": "authSchema"})
    assert doc == {"_id": "authSchema", "currentVersion": 5}


def test_find_with_no_match_returns_empty(client) -> None:
    """Filter for a non-existent _id → empty result."""
    docs = list(client["admin"]["system.version"].find({"_id": "doesnotexist"}))
    assert docs == []


def test_count(client) -> None:
    assert client["admin"]["system.version"].count_documents({}) == 1
    assert client["admin"]["system.version"].count_documents({"_id": "authSchema"}) == 1
    assert client["admin"]["system.version"].count_documents({"_id": "other"}) == 0


def test_aggregate_pipeline(client) -> None:
    """The view feeds through the aggregation engine like any other.
    The leading ``$match`` is lifted into the storage-level filter
    (same path the rest of aggregate uses)."""
    docs = list(
        client["admin"]["system.version"].aggregate([{"$match": {"currentVersion": {"$gte": 3}}}])
    )
    assert docs == [{"_id": "authSchema", "currentVersion": 5}]

    # And a $match that doesn't match returns empty.
    docs = list(
        client["admin"]["system.version"].aggregate([{"$match": {"currentVersion": {"$lt": 3}}}])
    )
    assert docs == []


def test_other_db_system_version_is_empty(client) -> None:
    """Only `admin.system.version` is the synthetic view. Other dbs'
    system.version namespace returns nothing."""
    docs = list(client["mydb"]["system.version"].find())
    assert docs == []


# ---------------------------------------------------------------------------
# Write path — rejected with code 13
# ---------------------------------------------------------------------------


def test_insert_rejected(client) -> None:
    with pytest.raises(OperationFailure) as exc_info:
        client["admin"]["system.version"].insert_one({"_id": "fake", "currentVersion": 99})
    assert exc_info.value.code == 13


def test_update_rejected(client) -> None:
    with pytest.raises(OperationFailure) as exc_info:
        client["admin"]["system.version"].update_one(
            {"_id": "authSchema"}, {"$set": {"currentVersion": 99}}
        )
    assert exc_info.value.code == 13


def test_delete_rejected(client) -> None:
    with pytest.raises(OperationFailure) as exc_info:
        client["admin"]["system.version"].delete_one({"_id": "authSchema"})
    assert exc_info.value.code == 13


def test_drop_collection_rejected(client) -> None:
    with pytest.raises(OperationFailure) as exc_info:
        client["admin"].drop_collection("system.version")
    assert exc_info.value.code == 13
