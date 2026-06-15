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
    assert any(e.get("client", "").startswith("127.0.0.1:") for e in conn_entries)


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


# ---- whatsmyuri -------------------------------------------------------------


def test_whatsmyuri_returns_real_peer(client: MongoClient) -> None:
    """``whatsmyuri`` returns the real connection peer (host:port),
    not a placeholder. Drivers and ``mongosh`` use this to identify
    which client they are."""
    out = client.admin.command("whatsmyuri")
    assert out["ok"] == 1.0
    you = out["you"]
    assert isinstance(you, str)
    # ``host:port`` shape; loopback in test fixtures.
    assert ":" in you
    host, port = you.rsplit(":", 1)
    assert host in ("127.0.0.1", "::1", "localhost")
    assert port.isdigit() and int(port) > 0


# ---- hostInfo ---------------------------------------------------------------


def test_host_info_returns_real_system_info(client: MongoClient) -> None:
    """``hostInfo`` reports real values from the running process —
    hostname, CPU arch, OS type/version, core count. Used to match
    against ``mongod``'s shape for monitoring tools."""
    import platform
    import socket

    out = client.admin.command("hostInfo")
    assert out["ok"] == 1.0

    system = out["system"]
    assert system["hostname"] == socket.gethostname()
    assert system["cpuArch"] == platform.machine()
    # Core count is at least 1; matches the number reported by os.cpu_count().
    import os

    assert system["numCores"] == (os.cpu_count() or 1)
    assert system["cpuAddrSize"] == 64

    os_block = out["os"]
    # platform.system() returns "Darwin" / "Linux" / "Windows"; the stub
    # always returned literal "secantus" — verify we no longer do.
    assert os_block["type"] == platform.system()
    assert os_block["type"] != "secantus"
    assert os_block["name"] == platform.system()


def test_host_info_memory_size_nonzero_on_posix(tmp_path) -> None:
    """On POSIX systems, ``memSizeMB`` reports the real physical RAM
    via sysconf, not 0. (Skipped when sysconf doesn't expose
    SC_PHYS_PAGES — e.g. on Windows runners — though SecantusDB
    isn't built there yet.)"""
    import os

    try:
        page_size = os.sysconf("SC_PAGE_SIZE")
        phys_pages = os.sysconf("SC_PHYS_PAGES")
    except (ValueError, OSError, AttributeError):
        pytest.skip("sysconf SC_PHYS_PAGES not available on this platform")

    if page_size <= 0 or phys_pages <= 0:
        pytest.skip("sysconf returned no usable memory size")

    # Pull through the server fixture for a realistic round-trip.
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        mc = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            out = mc.admin.command("hostInfo")
            assert out["system"]["memSizeMB"] > 0
        finally:
            mc.close()


# ---- serverStatus.mem (mongostat) -------------------------------------------


def test_server_status_has_mem_section(client: MongoClient) -> None:
    """mongostat dereferences ``mem.supported`` with no nil guard
    (mongo-tools status/readers.go ``ReadMapped``) — the section must
    always be present and mongod-shaped."""
    out = client.admin.command("serverStatus")
    mem = out["mem"]
    assert mem["supported"] is True
    assert mem["bits"] == 64
    assert mem["resident"] >= 0
    assert mem["virtual"] >= 0


# ---- top ---------------------------------------------------------------------


def test_top_reports_namespaces_in_mongod_shape(client: MongoClient) -> None:
    client["shop"]["items"].insert_one({"x": 1})
    out = client.admin.command("top")
    totals = out["totals"]
    assert totals["note"] == "all times in microseconds"
    ns = totals["shop.items"]
    for section in (
        "total",
        "readLock",
        "writeLock",
        "queries",
        "getmore",
        "insert",
        "update",
        "remove",
        "commands",
    ):
        assert set(ns[section]) == {"time", "count"}, section


def test_top_rejected_outside_admin(client: MongoClient) -> None:
    client["shop"]["items"].insert_one({"x": 1})
    with pytest.raises(OperationFailure) as exc:
        client["shop"].command("top")
    assert exc.value.code == 13


# ---- validate ---------------------------------------------------------------


def test_validate_collection_reports_clean(client: MongoClient) -> None:
    coll = client["valdb"]["things"]
    coll.insert_many([{"_id": i} for i in range(5)])
    coll.create_index([("x", 1)])

    out = client["valdb"].validate_collection("things")
    assert out["valid"] is True
    assert out["nrecords"] == 5
    assert out["nIndexes"] == 2
    assert set(out["keysPerIndex"]) == {"_id_", "x_1"}
    # full / scandata are accepted and ignored.
    assert client["valdb"].validate_collection("things", scandata=True, full=True)["valid"]


def test_validate_nonexistent_collection_errors(client: MongoClient) -> None:
    with pytest.raises(OperationFailure) as exc:
        client["valdb"].validate_collection("nope")
    assert exc.value.code == 26
