"""Tests for ``secantus.metrics`` and the ``serverStatus`` wire shape.

Two layers:

1. Unit tests on the :class:`Metrics` class — counters increment in the
   right buckets, snapshot is internally consistent.
2. End-to-end via pymongo — running real CRUD against a live server
   bumps the right opcounter buckets and connections accounting.
"""

from __future__ import annotations

import time

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer
from secantus.metrics import Metrics

# --- Unit: Metrics class ---------------------------------------------------


def test_metrics_starts_zeroed() -> None:
    m = Metrics()
    snap = m.snapshot()
    assert snap["connections"] == {
        "current": 0,
        "available": 0,
        "totalCreated": 0,
    }
    assert snap["opcounters"] == {
        "insert": 0,
        "query": 0,
        "update": 0,
        "delete": 0,
        "getmore": 0,
        "command": 0,
    }
    assert snap["network"]["numRequests"] == 0


def test_connection_lifecycle_tracks_current_and_total() -> None:
    m = Metrics()
    m.connection_opened()
    m.connection_opened()
    snap = m.snapshot()
    assert snap["connections"]["current"] == 2
    assert snap["connections"]["totalCreated"] == 2

    m.connection_closed()
    snap = m.snapshot()
    assert snap["connections"]["current"] == 1
    # totalCreated never decrements — it's a lifetime counter.
    assert snap["connections"]["totalCreated"] == 2


def test_record_command_buckets_match_mongod() -> None:
    """Each CRUD command name lands in its mongod-equivalent
    opcounters bucket; everything else is `command`."""
    m = Metrics()
    m.record_command("insert")
    m.record_command("find")
    m.record_command("update")
    m.record_command("delete")
    m.record_command("getMore")
    m.record_command("findAndModify")  # → update bucket
    m.record_command("count")  # → query bucket
    m.record_command("ping")  # → command bucket
    m.record_command("hello")  # → command bucket
    snap = m.snapshot()
    assert snap["opcounters"] == {
        "insert": 1,
        "query": 2,  # find + count
        "update": 2,  # update + findAndModify
        "delete": 1,
        "getmore": 1,
        "command": 2,  # ping + hello
    }
    # network.numRequests counts every dispatched command.
    assert snap["network"]["numRequests"] == 9


def test_uptime_advances() -> None:
    m = Metrics()
    snap1 = m.snapshot()
    time.sleep(0.05)
    snap2 = m.snapshot()
    # Snapshot millis grows; integer-second uptime may stay at 0
    # for fast tests — assert via the millis path.
    assert snap2["uptimeMillis"] >= snap1["uptimeMillis"] + 40


# --- Integration: serverStatus over the wire -------------------------------


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


def test_server_status_carries_secantus_marker(server: SecantusDBServer) -> None:
    """The `secantus` subdocument categorically identifies a SecantusDB
    server — real mongod never has the key. The conformance-gauge
    tripwire (pymongo_validation/plugin.py) relies on it to refuse to
    measure a foreign server."""
    import secantus

    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        status = mc["admin"].command("serverStatus")
    finally:
        mc.close()
    assert status["secantus"]["server"] == "python"
    assert status["secantus"]["version"] == secantus.__version__


def test_server_status_surfaces_real_uptime(server: SecantusDBServer) -> None:
    time.sleep(0.05)  # let some uptime accumulate
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        status = mc["admin"].command("serverStatus")
    finally:
        mc.close()
    assert "uptime" in status
    assert status["uptimeMillis"] >= 40
    # Envelope fields the spec requires.
    assert status["host"]
    assert status["version"]
    assert status["pid"] > 0


def test_server_status_opcounters_increment_under_real_traffic(
    server: SecantusDBServer,
) -> None:
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        coll = mc["metrics_test"]["c"]
        coll.insert_one({"_id": 1, "x": 1})
        coll.insert_many([{"_id": 2}, {"_id": 3}])
        list(coll.find())
        coll.update_one({"_id": 1}, {"$set": {"x": 2}})
        coll.delete_one({"_id": 2})

        status = mc["admin"].command("serverStatus")
    finally:
        mc.close()

    op = status["opcounters"]
    # Two `insert` commands (single + many; each is one wire command).
    assert op["insert"] == 2
    # `find` is a query.
    assert op["query"] >= 1
    assert op["update"] == 1
    assert op["delete"] == 1


def test_server_status_tracks_connections_total(server: SecantusDBServer) -> None:
    """`connections.totalCreated` grows monotonically across reconnects;
    `current` returns to baseline after clients disconnect."""
    mc1 = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        baseline = mc1["admin"].command("serverStatus")["connections"]["totalCreated"]
    finally:
        mc1.close()
    # Open + close another client.
    mc2 = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        # Force at least one round-trip so the connection registers.
        mc2["admin"].command("ping")
    finally:
        mc2.close()
    # Inspect via a third client (any open connection works).
    mc3 = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        after = mc3["admin"].command("serverStatus")["connections"]["totalCreated"]
    finally:
        mc3.close()
    assert after > baseline


def test_connection_status_surfaces_authenticated_user_roles(tmp_path) -> None:
    """`connectionStatus.authInfo.authenticatedUserRoles` reflects the
    role bindings the connection inherited at SCRAM completion. Without
    --auth on, the list is empty (legacy default-allow mode)."""
    from secantus.auth import SCRAM_SHA_256, derive_credentials

    with SecantusDBServer(
        host="127.0.0.1",
        port=0,
        storage_path=str(tmp_path / "wt"),
        require_auth=True,
    ) as srv:
        creds = derive_credentials("p")
        srv.storage.add_user(
            "admin",
            "alice",
            {
                "_id": "admin.alice",
                "user": "alice",
                "db": "admin",
                "credentials": creds.to_doc(),
                "roles": [{"role": "readWrite", "db": "shop"}],
                "mechanisms": [SCRAM_SHA_256],
            },
        )
        mc = MongoClient(
            srv.uri,
            username="alice",
            password="p",
            authSource="admin",
            authMechanism="SCRAM-SHA-256",
            serverSelectionTimeoutMS=2000,
        )
        try:
            status = mc["admin"].command("connectionStatus")
        finally:
            mc.close()
    assert status["authInfo"]["authenticatedUsers"] == [{"user": "alice", "db": "admin"}]
    assert status["authInfo"]["authenticatedUserRoles"] == [{"role": "readWrite", "db": "shop"}]
