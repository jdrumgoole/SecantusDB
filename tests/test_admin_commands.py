"""Tests for the admin-flavored mongod-shape commands.

`currentOp`, `fsync`, `getLog`, and `buildInfo` were either stubbed or
absent before this slice. They're driven via pymongo so the wire-shape
contract is what's exercised, not just the internal helpers.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

import secantus
from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer):
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield mc
    finally:
        mc.close()


# ---- buildInfo --------------------------------------------------------------


def test_build_info_reports_secantus_version(client: MongoClient) -> None:
    out = client.admin.command("buildInfo")
    # Driver-facing `version` stays at the MongoDB-compatibility value so
    # pymongo / mongo-go-driver / etc. enable the right feature flags.
    assert out["version"] == "7.0.0"
    # SecantusDB-specific marker lets admin tools tell which build they're
    # actually talking to.
    assert out["secantusVersion"] == secantus.__version__


# ---- getLog -----------------------------------------------------------------


def test_get_log_returns_recent_lines(server: SecantusDBServer, client: MongoClient) -> None:
    server.logs.append("I", "COMMAND", "test entry one", {"k": 1})
    server.logs.append("W", "STORAGE", "test entry two")
    out = client.admin.command("getLog", "global")
    assert out["ok"] == 1.0
    # mongod returns lines as strings; ours encode level + component + msg.
    assert any("test entry one" in line for line in out["log"])
    assert any("test entry two" in line for line in out["log"])
    assert out["totalLinesWritten"] >= 2


# ---- fsync ------------------------------------------------------------------


def test_fsync_runs_checkpoint(client: MongoClient) -> None:
    # Insert a doc, fsync, verify the response shape. Persistence is
    # exercised separately in tests/test_storage.py.
    client["fsync_db"]["c"].insert_one({"_id": 1})
    out = client.admin.command("fsync")
    assert out["ok"] == 1.0
    assert out["numFiles"] == 1


def test_fsync_with_lock_true_rejected(client: MongoClient) -> None:
    with pytest.raises(OperationFailure) as exc:
        client.admin.command({"fsync": 1, "lock": True})
    # Code should be a recognisable refusal — we use 9 (Location9) per the
    # error contract; pin the exact value so behavior doesn't drift.
    assert exc.value.code == 9


# ---- currentOp --------------------------------------------------------------


def test_current_op_lists_calling_connection(client: MongoClient) -> None:
    out = client.admin.command("currentOp")
    assert out["ok"] == 1.0
    assert "inprog" in out
    # The pymongo connection making this very call is one of the entries.
    conn_entries = [e for e in out["inprog"] if e.get("type") == "op"]
    assert any(
        e.get("client", "").startswith("127.0.0.1:") for e in conn_entries
    )


def test_current_op_lists_open_cursors(client: MongoClient) -> None:
    coll = client["currentop_db"]["c"]
    coll.insert_many([{"i": i} for i in range(20)])
    # Open a cursor and read one batch — leaves a server-side cursor open.
    cursor = coll.find().batch_size(2)
    next(cursor)
    try:
        out = client.admin.command("currentOp")
        idle_cursors = [e for e in out["inprog"] if e.get("type") == "idleCursor"]
        assert len(idle_cursors) >= 1
        c = idle_cursors[0]
        assert c["ns"] == "currentop_db.c"
        assert c["cursorId"] != 0
    finally:
        cursor.close()
