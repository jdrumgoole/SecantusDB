"""Smoke test: pymongo against the embedded Rust server (R6).

This is the embryonic R8 conformance gate — the first test that drives the
**Rust server** (its own wire / dispatch / cursors over WiredTiger) through a
real `pymongo` client, exactly as the full suites will once the server grows the
remaining command families.

Scoped to the commands the Rust dispatch currently implements: handshake
(`hello` / `ping`), `insert`, `find` (+ `getMore` / `killCursors`), `delete`,
and `aggregate` (incl. `count_documents`, which pymongo routes through an
aggregation pipeline). **Not yet exercised** (deferred): storage-backed
aggregation stages (`$lookup` / `$out` / `$merge`), change streams, etc.

Gated on the `_secantus_server` extension being importable, which requires the
WiredTiger-linking build (the wheel's CMake under
``SECANTUS_BUILD_STORAGE_ENGINE=ON`` or a local ``maturin`` build with
``SECANTUS_WT_INCLUDE`` / ``SECANTUS_WT_LIB`` set). It is skipped in the WT-less
dev sandbox / the default `rust` CI job.
"""

from __future__ import annotations

import shutil
import subprocess

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")


def _client(srv):
    host, port = srv.address
    return pymongo.MongoClient(
        host,
        port,
        directConnection=True,
        serverSelectionTimeoutMS=5000,
    )


def test_pymongo_crud_against_rust_server(tmp_path) -> None:
    """Insert / find / find-with-filter / count_documents / delete end-to-end."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]

        coll.insert_many([{"_id": 1, "x": 1}, {"_id": 2, "x": 2}, {"_id": 3, "x": 1}])
        assert len(list(coll.find({}))) == 3
        assert coll.find_one({"_id": 2})["x"] == 2
        assert sorted(d["_id"] for d in coll.find({"x": 1})) == [1, 3]
        # count_documents routes through an aggregation pipeline.
        assert coll.count_documents({}) == 3
        assert coll.count_documents({"x": 1}) == 2

        coll.delete_one({"_id": 1})
        assert sorted(d["_id"] for d in coll.find({})) == [2, 3]
    finally:
        srv.stop()


def test_find_one_and_update_against_rust_server(tmp_path) -> None:
    """findAndModify via pymongo's find_one_and_update (old + new images)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "x": 1})
        # default returns the pre-image
        old = coll.find_one_and_update({"_id": 1}, {"$set": {"x": 2}})
        assert old["x"] == 1
        # ReturnDocument.AFTER returns the post-image
        new = coll.find_one_and_update(
            {"_id": 1}, {"$set": {"x": 3}}, return_document=pymongo.ReturnDocument.AFTER
        )
        assert new["x"] == 3
        # removed
        removed = coll.find_one_and_delete({"_id": 1})
        assert removed["x"] == 3
        assert coll.find_one({"_id": 1}) is None
    finally:
        srv.stop()


def test_aggregate_pipeline_against_rust_server(tmp_path) -> None:
    """A direct aggregation pipeline ($match → $group) via pymongo."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many(
            [
                {"_id": 1, "g": "a", "v": 10},
                {"_id": 2, "g": "a", "v": 20},
                {"_id": 3, "g": "b", "v": 5},
            ]
        )
        result = list(
            coll.aggregate(
                [
                    {"$match": {"g": "a"}},
                    {"$group": {"_id": "$g", "total": {"$sum": "$v"}}},
                ]
            )
        )
        assert result == [{"_id": "a", "total": 30}]
    finally:
        srv.stop()


def test_rust_server_handshake(tmp_path) -> None:
    """A bare ``hello`` / ``ping`` admin round-trip — the handshake the driver
    runs on connect."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        admin = _client(srv).admin
        hello = admin.command("hello")
        assert hello["ok"] == 1.0
        assert hello["isWritablePrimary"] is True
        assert admin.command("ping")["ok"] == 1.0
    finally:
        srv.stop()


def test_scram_auth_roundtrip_against_rust_server(tmp_path) -> None:
    """createUser + a SCRAM-SHA-256 authenticated reconnect (R5b).

    The Rust server doesn't yet *enforce* ``--auth`` (RBAC gating is R5b-2), but
    a pymongo client given credentials performs the full
    ``saslStart`` → ``saslContinue`` SCRAM-SHA-256 handshake on connect. A wrong
    password must surface as ``OperationFailure`` (auth failed); the right one
    must let commands through.
    """
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        host, port = srv.address
        admin = _client(srv).admin
        created = admin.command(
            "createUser",
            "alice",
            pwd="s3cr3t",
            roles=[{"role": "readWrite", "db": "t"}],
        )
        assert created["ok"] == 1.0

        # Right password → the SCRAM handshake succeeds and commands run.
        good = pymongo.MongoClient(
            host,
            port,
            username="alice",
            password="s3cr3t",
            authSource="admin",
            authMechanism="SCRAM-SHA-256",
            directConnection=True,
            serverSelectionTimeoutMS=5000,
        )
        try:
            assert good.admin.command("ping")["ok"] == 1.0
            good["t"]["c"].insert_one({"_id": 1, "ok": True})
            assert good["t"]["c"].find_one({"_id": 1})["ok"] is True
        finally:
            good.close()

        # Wrong password → SCRAM proof fails → OperationFailure on first command.
        bad = pymongo.MongoClient(
            host,
            port,
            username="alice",
            password="WRONG",
            authSource="admin",
            authMechanism="SCRAM-SHA-256",
            directConnection=True,
            serverSelectionTimeoutMS=5000,
        )
        try:
            with pytest.raises(pymongo.errors.OperationFailure):
                bad.admin.command("ping")
        finally:
            bad.close()

        # usersInfo round-trips and hides credentials by default.
        info = admin.command("usersInfo", "alice")
        assert len(info["users"]) == 1
        assert info["users"][0]["user"] == "alice"
        assert "credentials" not in info["users"][0]

        # dropUser removes it.
        assert admin.command("dropUser", "alice")["ok"] == 1.0
        assert admin.command("usersInfo", "alice")["users"] == []
    finally:
        srv.stop()


def test_custom_roles_against_rust_server(tmp_path) -> None:
    """createRole / rolesInfo / updateRole / dropRole over WiredTiger (R5b-3).

    Exercises the custom-role storage round-trip through the real WT adapter on
    an auth-off server (no gating needed). The privilege-resolution + inheritance
    paths are covered by the Rust unit tests.
    """
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        admin = _client(srv).admin
        created = admin.command(
            "createRole",
            "appReader",
            privileges=[{"resource": {"db": "app", "collection": ""}, "actions": ["find"]}],
            roles=[],
        )
        assert created["ok"] == 1.0

        # rolesInfo returns the stored shape.
        info = admin.command("rolesInfo", "appReader")
        assert len(info["roles"]) == 1
        assert info["roles"][0]["role"] == "appReader"

        # a built-in name can't be redefined
        with pytest.raises(pymongo.errors.OperationFailure):
            admin.command("createRole", "read", privileges=[], roles=[])

        # updateRole replaces privileges in place
        assert (
            admin.command(
                "updateRole",
                "appReader",
                privileges=[{"resource": {"db": "app", "collection": ""}, "actions": ["insert"]}],
            )["ok"]
            == 1.0
        )

        # dropRole removes it
        assert admin.command("dropRole", "appReader")["ok"] == 1.0
        with pytest.raises(pymongo.errors.OperationFailure):
            admin.command("dropRole", "appReader")  # RoleNotFound
    finally:
        srv.stop()


def test_tls_against_rust_server(tmp_path) -> None:
    """End-to-end TLS: a pymongo client connects over an encrypted channel to a
    TLS-enabled Rust server (R5c).

    The TLS transport itself is also covered deterministically by the Rust
    integration test (`crates/secantus-server/tests/tls.rs`); this adds driver
    coverage. Skipped where `openssl` isn't available to mint a self-signed cert.
    """
    if shutil.which("openssl") is None:
        pytest.skip("openssl not available to generate a test certificate")
    cert = tmp_path / "cert.pem"
    key = tmp_path / "key.pem"
    subprocess.run(
        [
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            str(key),
            "-out",
            str(cert),
            "-days",
            "1",
            "-nodes",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost,IP:127.0.0.1",
        ],
        check=True,
        capture_output=True,
    )
    srv = _server.RustServer(
        str(tmp_path / "wt"),
        0,
        tls_cert_file=str(cert),
        tls_key_file=str(key),
    )
    try:
        _, port = srv.address
        client = pymongo.MongoClient(
            "127.0.0.1",
            port,
            tls=True,
            tlsCAFile=str(cert),
            directConnection=True,
            serverSelectionTimeoutMS=5000,
        )
        try:
            assert client.admin.command("ping")["ok"] == 1.0
            client["t"]["c"].insert_one({"_id": 1, "x": 1})
            assert client["t"]["c"].find_one({"_id": 1})["x"] == 1
        finally:
            client.close()
    finally:
        srv.stop()


def test_role_grant_revoke_and_update_user(tmp_path) -> None:
    """grant/revoke quartet + updateUser over WiredTiger (R5b-4)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        admin = _client(srv).admin
        admin.command("createRole", "r", privileges=[], roles=[])
        # grant privileges, then read them back via rolesInfo
        admin.command(
            "grantPrivilegesToRole",
            "r",
            privileges=[{"resource": {"db": "app", "collection": ""}, "actions": ["find"]}],
        )
        rec = admin.command("rolesInfo", "r")["roles"][0]
        assert rec["privileges"][0]["actions"] == ["find"]
        # revoke it → privilege dropped
        admin.command(
            "revokePrivilegesFromRole",
            "r",
            privileges=[{"resource": {"db": "app", "collection": ""}, "actions": ["find"]}],
        )
        assert admin.command("rolesInfo", "r")["roles"][0]["privileges"] == []

        # updateUser rotates a password
        admin.command("createUser", "u", pwd="old", roles=[{"role": "read", "db": "app"}])
        assert admin.command("updateUser", "u", pwd="new")["ok"] == 1.0

        # dropAllUsersFromDatabase clears the db
        n = admin.command("dropAllUsersFromDatabase", 1)["n"]
        assert n == 1
        assert admin.command("usersInfo", 1)["users"] == []
    finally:
        srv.stop()


def test_require_auth_gating_against_rust_server(tmp_path) -> None:
    """End-to-end `--auth` gating (R5b-2).

    With access control on, the driver handshake (`hello` / `ping`) still flows
    on an unauthenticated connection, but a data command is rejected with
    Unauthorized (pymongo surfaces it as ``OperationFailure``). The
    authenticated-success and per-action RBAC paths are covered exhaustively by
    the Rust unit tests; this asserts the gating boundary over a real socket.

    (We don't drive the authenticated path here: seeding the first user requires
    either a localhost exception or reopening the same on-disk storage, and
    WiredTiger only allows one open of a home directory per process.)
    """
    srv = _server.RustServer(str(tmp_path / "wt"), 0, require_auth=True)
    try:
        client = _client(srv)
        # Pre-auth handshake commands are allowed without authentication.
        assert client.admin.command("ping")["ok"] == 1.0
        assert client.admin.command("hello")["ok"] == 1.0
        # A data command without authentication is rejected.
        with pytest.raises(pymongo.errors.OperationFailure):
            client["app"]["c"].find_one({})
    finally:
        srv.stop()


def test_admin_commands_against_rust_server(tmp_path) -> None:
    """DDL + introspection + db-admin via pymongo: listCollections, createIndexes
    / listIndexes, dbStats, serverStatus, drop."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        client = _client(srv)
        db = client["t"]
        db.c.insert_one({"_id": 1, "x": 1})  # auto-creates the collection
        assert "c" in db.list_collection_names()

        db.c.create_index([("x", 1)])
        index_names = [ix["name"] for ix in db.c.list_indexes()]
        assert "_id_" in index_names
        assert any(n.startswith("x_") for n in index_names)

        stats = db.command("dbStats")
        assert stats["ok"] == 1.0 and stats["db"] == "t"
        status = client.admin.command("serverStatus")
        assert status["ok"] == 1.0
        # Categorical SecantusDB marker (gauge tripwire depends on it).
        assert status["secantus"]["server"] == "rust"

        db.c.drop()
        assert "c" not in db.list_collection_names()
    finally:
        srv.stop()


def test_change_stream_against_rust_server(tmp_path) -> None:
    """R3b: a collection change stream on the Rust server sees insert / update /
    delete events with documentKey, updateDescription, and updateLookup
    fullDocument. Needs the replica-set persona ($changeStream is rejected with
    40573 on a standalone).

    Deterministic under parallel execution: ``coll.watch()`` runs its aggregate
    synchronously, so the tailable cursor is registered *before* the writes —
    no sleep/thread race. ``try_next`` polls (R3b-a's getMore is non-blocking)
    with a wall-clock deadline rather than blocking forever on a missing event.
    """
    import time

    srv = _server.RustServer(str(tmp_path / "wt"), 0, host="127.0.0.1", replica_set_name="secantus")
    try:
        coll = _client(srv)["csdb"]["c"]
        coll.insert_one({"_id": 1, "n": 0})  # create the collection

        # watch() opens the cursor here; writes after this point are captured.
        cs = coll.watch(full_document="updateLookup", max_await_time_ms=500)
        try:
            # Note: updateLookup reads the doc at EVENT-READ time, not update
            # time (mongod's documented "most current majority-committed
            # version" semantics — attach_full_document does a live find). So
            # the updated _id:1 must survive to the poll below; only _id:2 is
            # deleted.
            coll.update_one({"_id": 1}, {"$set": {"n": 5}})
            coll.insert_one({"_id": 2})
            coll.delete_one({"_id": 2})

            events: list = []
            deadline = time.monotonic() + 30
            while len(events) < 3 and time.monotonic() < deadline:
                ev = cs.try_next()
                if ev is not None:
                    events.append(ev)

            ops = [e["operationType"] for e in events]
            assert ops == ["update", "insert", "delete"], ops
            assert events[0]["updateDescription"]["updatedFields"] == {"n": 5}
            assert events[0]["fullDocument"] == {"_id": 1, "n": 5}
            assert events[1]["documentKey"] == {"_id": 2}
            assert events[2]["documentKey"] == {"_id": 2}
            # Each event carries a resume token under _id.
            assert all("_id" in e for e in events)
        finally:
            cs.close()
    finally:
        srv.stop()
