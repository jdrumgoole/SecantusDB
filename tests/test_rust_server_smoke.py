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
bson = pytest.importorskip("bson")


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


def test_ops_on_never_created_collections_are_noops(tmp_path) -> None:
    """Every operation on a collection whose shard was never written must be a
    clean no-op/empty result, not a WiredTiger "No such file" error.

    Regression: lazy shard creation makes a collection's documents shard exist
    only once written to, so read / drop / delete / aggregate paths must tolerate
    an absent shard. ``drop()`` on a never-created collection in particular went
    through ``purge_collection_tables``, which opened the (absent) shard cursor
    and failed — caught only by the PGO release workload, whose first op is
    exactly ``coll.drop()`` on a fresh collection.
    """
    srv = _server.RustServer(str(tmp_path / "wt"), 0, host="127.0.0.1", replica_set_name="secantus")
    try:
        db = _client(srv)["fresh"]
        # None of these has ever been written to → no shard exists yet.
        db.a.drop()  # the op that broke the binary's PGO build
        assert list(db.b.find({})) == []
        assert list(db.c.find({"v": {"$gte": 1, "$lt": 5}})) == []
        assert db.d.count_documents({}) == 0
        assert db.e.estimated_document_count() == 0
        assert db.f.distinct("x") == []
        assert db.g.delete_many({"n": {"$lt": 5}}).deleted_count == 0
        assert db.h.update_many({"n": {"$lt": 5}}, {"$set": {"t": 1}}).modified_count == 0
        assert list(db.i.aggregate([{"$match": {"v": 1}}])) == []
        assert list(db.j.aggregate([{"$group": {"_id": "$g", "n": {"$sum": 1}}}])) == []
        assert db.k.find_one_and_update({"x": 1}, {"$set": {"y": 2}}) is None
        # create an index, then drop the still-empty collection.
        db.m.create_index("v")
        db.m.drop()
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


def test_secantus_admin_prune_commands(tmp_path) -> None:
    """secantusAdmin.pruneOplog / pruneTtl return {pruned, ok} against the Rust
    server (issue #163 — the native maintenance commands the admin UI drives).
    """
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        client = _client(srv)
        # Seed a little data so the oplog + a collection exist.
        client["t"]["c"].insert_many([{"_id": i} for i in range(3)])

        r1 = client.admin.command({"secantusAdmin.pruneOplog": 1})
        assert r1["ok"] == 1.0
        assert isinstance(r1["pruned"], int) and r1["pruned"] >= 0

        r2 = client.admin.command({"secantusAdmin.pruneTtl": 1})
        assert r2["ok"] == 1.0
        assert isinstance(r2["pruned"], int) and r2["pruned"] >= 0
    finally:
        srv.stop()


def test_secantus_admin_restore_archive_roundtrips(tmp_path) -> None:
    """backupArchive -> restoreArchive round-trips data through a fresh dir, and
    the restored directory is a startable WT home (issue #163)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    archive = str(tmp_path / "backup.tar.gz")
    target = str(tmp_path / "restored")
    try:
        client = _client(srv)
        client["t"]["c"].insert_many([{"_id": i, "x": i * 2} for i in range(5)])
        r = client.admin.command({"secantusAdmin.backupArchive": 1, "outputPath": archive})
        assert r["ok"] == 1.0

        rr = client.admin.command(
            {"secantusAdmin.restoreArchive": 1, "archivePath": archive, "targetDir": target}
        )
        assert rr["ok"] == 1.0
        assert rr["fileCount"] > 0
        assert rr["targetDir"] and rr["archive"]
    finally:
        srv.stop()

    # The restored directory is a startable WT home carrying the data.
    srv2 = _server.RustServer(target, 0)
    try:
        c2 = _client(srv2)["t"]["c"]
        assert sorted(d["_id"] for d in c2.find({})) == [0, 1, 2, 3, 4]
        assert c2.find_one({"_id": 3})["x"] == 6
    finally:
        srv2.stop()


def test_restore_archive_rejects_nonempty_target(tmp_path) -> None:
    """A non-empty target without allowExisting is IllegalOperation(20)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    archive = str(tmp_path / "b.tar.gz")
    target = tmp_path / "restored"
    target.mkdir()
    (target / "sentinel").write_text("x")
    try:
        client = _client(srv)
        client["t"]["c"].insert_one({"_id": 1})
        client.admin.command({"secantusAdmin.backupArchive": 1, "outputPath": archive})
        with pytest.raises(pymongo.errors.OperationFailure) as ei:
            client.admin.command(
                {
                    "secantusAdmin.restoreArchive": 1,
                    "archivePath": archive,
                    "targetDir": str(target),
                }
            )
        assert ei.value.code == 20
    finally:
        srv.stop()


def test_secantus_grant_revoke_roles_to_user(tmp_path) -> None:
    """grantRolesToUser / revokeRolesFromUser modify a user's roles on the Rust
    server, reflected by usersInfo (issue #163)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        admin = _client(srv)["admin"]
        admin.command("createUser", "carol", pwd="pw", roles=[{"role": "read", "db": "admin"}])

        admin.command("grantRolesToUser", "carol", roles=[{"role": "readWrite", "db": "admin"}])
        info = admin.command("usersInfo", "carol")
        roles = {(r["role"], r["db"]) for r in info["users"][0]["roles"]}
        assert roles == {("read", "admin"), ("readWrite", "admin")}

        admin.command("revokeRolesFromUser", "carol", roles=[{"role": "read", "db": "admin"}])
        info2 = admin.command("usersInfo", "carol")
        roles2 = {(r["role"], r["db"]) for r in info2["users"][0]["roles"]}
        assert roles2 == {("readWrite", "admin")}
    finally:
        srv.stop()


def test_kill_op_closes_a_connection(tmp_path) -> None:
    """killOp closes another connection by its conn_id (from hello.connectionId)
    and reports {info, ok}; a bogus opid is reported not-found (issue #163)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        c1 = _client(srv)
        cid1 = c1.admin.command("hello")["connectionId"]
        c2 = _client(srv)

        killed = c2.admin.command({"killOp": 1, "op": cid1})
        assert killed["ok"] == 1.0
        assert killed["info"] == "operation killed"

        bogus = c2.admin.command({"killOp": 1, "op": 2_000_000_000})
        assert bogus["info"] == "no operation with that opid"

        c1.close()
        c2.close()
    finally:
        srv.stop()


def test_get_log_returns_connection_lines(tmp_path) -> None:
    """getLog surfaces the server's in-memory log ring buffer, including the
    connection-accept line (issue #163)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        client = _client(srv)
        client.admin.command("ping")  # ensure a connection is established
        res = client.admin.command({"getLog": "global"})
        assert res["ok"] == 1.0
        assert res["totalLinesWritten"] >= 1
        assert any("connection accepted" in line for line in res["log"])
    finally:
        srv.stop()


def test_geo_near_index_optimization_against_rust_server(tmp_path) -> None:
    """A leading bounded ``$geoNear`` rides the geo index (conservative
    ``$geoWithin`` candidate fetch) on the Rust server; output must be identical
    to the brute-force scan. Compare an indexed collection against an unindexed
    one over many random queries."""
    import random

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        db = _client(srv)["t"]
        rng = random.Random(2024)
        docs = [
            {
                "_id": i,
                "loc": {
                    "type": "Point",
                    "coordinates": [rng.uniform(-15, 15), rng.uniform(-15, 15)],
                },
                "v": rng.randint(0, 5),
            }
            for i in range(400)
        ]
        db.idx.insert_many(docs)
        db.noidx.insert_many(docs)
        db.idx.create_index([("loc", "2dsphere")])  # only this one gets optimized

        for _ in range(40):
            cx, cy = rng.uniform(-15, 15), rng.uniform(-15, 15)
            max_d = rng.uniform(50_000, 1_500_000)
            stage = {
                "$geoNear": {
                    "near": {"type": "Point", "coordinates": [cx, cy]},
                    "distanceField": "d",
                    "maxDistance": max_d,
                    "key": "loc",
                    "spherical": True,
                }
            }
            pipeline: list[dict] = [stage]
            if rng.random() < 0.4:
                pipeline.append({"$match": {"v": {"$gte": 2}}})
            if rng.random() < 0.3:
                stage["$geoNear"]["query"] = {"v": {"$lte": 4}}
            opt = [(d["_id"], round(d["d"], 6)) for d in db.idx.aggregate(pipeline)]
            brute = [(d["_id"], round(d["d"], 6)) for d in db.noidx.aggregate(pipeline)]
            assert opt == brute, f"center={(cx, cy)} maxDistance={max_d}"
    finally:
        srv.stop()


def test_lookup_index_order_against_rust_server(tmp_path) -> None:
    """A simple `$lookup` whose foreign collection has a leading-field index on
    `foreignField` drives a per-outer-doc index probe on the Rust server, so the
    `as` array comes back in index order (not foreign-scan order) — matching the
    Python server. Foreign docs are inserted out of index order to make the
    distinction observable."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        db = _client(srv)["t"]
        db.o.insert_one({"_id": 1, "k": 1})
        # inserted in a NON-(fk,tag) order; the compound index reorders to a,b,c
        db.f.insert_many(
            [
                {"_id": 10, "fk": 1, "tag": "c"},
                {"_id": 11, "fk": 1, "tag": "a"},
                {"_id": 12, "fk": 1, "tag": "b"},
            ]
        )
        db.f.create_index([("fk", 1), ("tag", 1)])
        res = list(
            db.o.aggregate(
                [{"$lookup": {"from": "f", "localField": "k", "foreignField": "fk", "as": "m"}}]
            )
        )
        # Index order by (fk, tag): a, b, c — not the c, a, b insertion order.
        assert [m["tag"] for m in res[0]["m"]] == ["a", "b", "c"]

        # An array local value uses $in and still matches all elements.
        db.o2.insert_one({"_id": 2, "k": [1]})
        db.f2.insert_many([{"_id": 20, "fk": 1}, {"_id": 21, "fk": 2}])
        db.f2.create_index([("fk", 1)])
        res2 = list(
            db.o2.aggregate(
                [{"$lookup": {"from": "f2", "localField": "k", "foreignField": "fk", "as": "m"}}]
            )
        )
        assert [m["_id"] for m in res2[0]["m"]] == [20]
    finally:
        srv.stop()


def test_write_concern_validation_against_rust_server(tmp_path) -> None:
    """The Rust server rejects a malformed `writeConcern` before running a write
    command, with mongod's codes (matching the Python server): negative/too-large
    integer `w` → FailedToParse (9), unknown string `w` → UnknownReplWriteConcern
    (79), a bool / non-number-or-string `w` → TypeMismatch (14). A well-formed
    (or absent) writeConcern is accepted; `w > 1` still succeeds (the single-node
    writeConcernError is attached, not an error)."""
    import pymongo.errors

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        db = _client(srv)["t"]
        rejects = [
            ({"w": -5}, 9),
            ({"w": 99}, 9),
            ({"w": "nope"}, 79),
            ({"w": 1.5}, 14),
            ({"w": True}, 14),
            ({"j": "x"}, 14),
        ]
        for wc, code in rejects:
            with pytest.raises(pymongo.errors.OperationFailure) as exc:
                db.command("insert", "c", documents=[{"x": 1}], writeConcern=wc)
            assert exc.value.code == code, f"wc={wc} expected {code} got {exc.value.code}"

        # Well-formed / satisfiable writeConcerns are accepted.
        for wc in [{"w": 1}, {"w": "majority"}, {"j": True}, {"wtimeout": 100}, {"w": 2}]:
            r = db.command("insert", "c", documents=[{"x": 1}], writeConcern=wc)
            assert r["ok"] == 1.0, f"wc={wc} should succeed"
    finally:
        srv.stop()


def test_synthetic_view_write_rejected_against_rust_server(tmp_path) -> None:
    """A direct insert / update / delete on a synthetic read-only view
    (`local.oplog.rs` / `admin.system.users`) is rejected with code 13 on the Rust
    server (matching the Python server / mongod), while a regular collection write
    still succeeds."""
    import pymongo.errors

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        client = _client(srv)
        for db_name, coll in [("local", "oplog.rs"), ("admin", "system.users")]:
            db = client[db_name]
            for op, args in [
                ("insert", {"documents": [{"x": 1}]}),
                ("update", {"updates": [{"q": {}, "u": {"$set": {"x": 1}}}]}),
                ("delete", {"deletes": [{"q": {}, "limit": 0}]}),
            ]:
                with pytest.raises(pymongo.errors.OperationFailure) as exc:
                    db.command(op, coll, **args)
                assert exc.value.code == 13, f"{db_name}.{coll} {op}: got {exc.value.code}"

        # A regular collection write is unaffected.
        r = client["t"].command("insert", "c", documents=[{"x": 1}])
        assert r["ok"] == 1.0
    finally:
        srv.stop()


def test_view_reads_resolve_against_rust_server(tmp_path) -> None:
    """find / aggregate / count on a view resolve the view's pipeline against its
    base collection on the Rust server (previously they returned nothing) —
    including filter/sort/skip/limit/projection and a view-on-a-view."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        db = _client(srv)["t"]
        db.src.insert_many([{"_id": i, "a": i % 3, "v": i} for i in range(9)])
        db.command("create", "vw", viewOn="src", pipeline=[{"$match": {"a": 1}}])

        assert sorted(d["_id"] for d in db.vw.find({})) == [1, 4, 7]
        assert sorted(d["_id"] for d in db.vw.find({"v": {"$gt": 3}})) == [4, 7]
        assert [d["_id"] for d in db.vw.find({}).sort("_id", -1).limit(2)] == [7, 4]
        assert [d["_id"] for d in db.vw.find({}).sort("_id", 1).skip(1)] == [4, 7]
        assert db.vw.find_one({"_id": 4}, {"v": 1, "_id": 0}) == {"v": 4}
        assert [d["_id"] for d in db.vw.aggregate([{"$sort": {"_id": 1}}])] == [1, 4, 7]
        assert db.vw.count_documents({}) == 3
        assert db.vw.count_documents({"v": {"$gt": 3}}) == 2

        db.command("create", "vw2", viewOn="vw", pipeline=[{"$match": {"v": {"$gt": 3}}}])
        assert sorted(d["_id"] for d in db.vw2.find({})) == [4, 7]
        assert db.vw2.count_documents({}) == 2
    finally:
        srv.stop()


def test_aggregate_stage_name_validation_against_rust_server(tmp_path) -> None:
    """The Rust server validates aggregation stage names up-front (matching the
    Python server / mongod): an unrecognized stage → Location40324, an Atlas-only
    stage → CommandNotSupported (115). Recognized stages run normally."""
    import pymongo.errors

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        db = _client(srv)["t"]
        db.c.insert_many([{"_id": i, "a": i % 3} for i in range(6)])

        for pipeline, code in [
            ([{"$badStage": {}}], 40324),
            ([{"$match": {"a": 1}}, {"$nope": 1}], 40324),
            ([{"$search": {}}], 115),
            ([{"$vectorSearch": {}}], 115),
            ([{"$listSearchIndexes": {}}], 115),
        ]:
            with pytest.raises(pymongo.errors.OperationFailure) as exc:
                list(db.c.aggregate(pipeline))
            assert exc.value.code == code, f"{pipeline}: expected {code} got {exc.value.code}"

        # A recognized pipeline still runs.
        got = sorted(
            (d["_id"], d["n"])
            for d in db.c.aggregate([{"$group": {"_id": "$a", "n": {"$sum": 1}}}])
        )
        assert got == [(0, 2), (1, 2), (2, 2)]
    finally:
        srv.stop()


def test_unknown_expression_operator_error_codes(tmp_path) -> None:
    """Context-specific unknown-operator codes on the Rust server, matching the
    Python server and mongod 6.0: 168 InvalidPipelineOperator for a query
    ``$expr``; Location31325 inside an aggregation ``$project``. A
    projection-only operator ($slice/$elemMatch/$meta shape) is never
    mislabeled as an unknown expression."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": 5, "arr": [1, 2, 3]})

        with pytest.raises(pymongo.errors.OperationFailure) as expr_exc:
            list(coll.find({"$expr": {"$notreal": [1, 2]}}))
        assert expr_exc.value.code == 168

        with pytest.raises(pymongo.errors.OperationFailure) as proj_exc:
            list(coll.aggregate([{"$project": {"y": {"$notreal": ["$a"]}}}]))
        assert proj_exc.value.code == 31325
        assert "Unknown expression $notreal" in proj_exc.value.details["errmsg"]

        # Nested unknown operator is found too.
        with pytest.raises(pymongo.errors.OperationFailure) as nested_exc:
            list(coll.aggregate([{"$project": {"y": {"$add": [1, {"$bogus": 2}]}}}]))
        assert nested_exc.value.code == 31325

        # $slice in its projection-only shape still projects (not an expression).
        got = list(coll.aggregate([{"$project": {"arr": {"$slice": ["$arr", 2]}}}]))
        assert got[0]["arr"] == [1, 2]
    finally:
        srv.stop()


def test_embedded_handle_wiredtiger_knobs(tmp_path) -> None:
    """The embedded handle exposes the daemon's WiredTiger knobs: cache_size /
    session_max / sync_on_commit thread into wt_config, and a server opened
    with non-default values works end-to-end."""
    srv = _server.RustServer(
        str(tmp_path / "wt"),
        0,
        cache_size="256M",
        session_max=100,
        sync_on_commit=True,
    )
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": i, "v": i} for i in range(20)])
        assert coll.count_documents({}) == 20
        assert coll.find_one({"_id": 7})["v"] == 7
    finally:
        srv.stop()


def test_json_schema_keyword_validation(tmp_path) -> None:
    """$jsonSchema keyword validation on the Rust server, matching the Python
    server and a mongod 7.0 probe: metadata keywords accepted; unsupported /
    unknown keywords rejected at parse time (9 FailedToParse, even before any
    document is scanned); draft-4 exclusive bounds and multipleOf /
    tuple-items semantics."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": 1, "n": 6, "arr": [1, "x"]}, {"_id": 2, "n": 7.5, "arr": [1]}])

        assert len(list(coll.find({"$jsonSchema": {"title": "t", "description": "d"}}))) == 2
        got = [
            d["_id"]
            for d in coll.find(
                {"$jsonSchema": {"properties": {"n": {"minimum": 6, "exclusiveMinimum": True}}}}
            )
        ]
        assert got == [2]
        assert [
            d["_id"] for d in coll.find({"$jsonSchema": {"properties": {"n": {"multipleOf": 2.5}}}})
        ] == [2]
        assert [
            d["_id"]
            for d in coll.find(
                {
                    "$jsonSchema": {
                        "properties": {
                            "arr": {"items": [{"bsonType": "int"}], "additionalItems": False}
                        }
                    }
                }
            )
        ] == [2]

        for schema, code, frag in [
            ({"$ref": "#/x"}, 9, "not currently supported"),
            ({"notakeyword": 1}, 9, "Unknown $jsonSchema keyword: notakeyword"),
            ({"properties": {"n": {"notakeyword": 1}}}, 9, "Unknown $jsonSchema keyword"),
            ({"minimum": 5, "exclusiveMinimum": 6}, 14, "must be a boolean"),
            ({"exclusiveMinimum": True}, 9, "must be a present if"),
            ({"multipleOf": 0}, 9, "positive value"),
            ({"title": 5}, 14, "must be of type string"),
        ]:
            with pytest.raises(pymongo.errors.OperationFailure) as exc:
                list(coll.find({"$jsonSchema": schema}))
            assert exc.value.code == code, schema
            assert frag in exc.value.details["errmsg"], schema
    finally:
        srv.stop()


def test_median_and_percentile_accumulators(tmp_path) -> None:
    """$median / $percentile on the Rust server (group + expression forms) —
    mongod's discrete percentile, doubles out, matching the Python server."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"x": v} for v in [10, 20, 30, 40]])
        r = list(
            coll.aggregate(
                [
                    {
                        "$group": {
                            "_id": None,
                            "med": {"$median": {"input": "$x", "method": "approximate"}},
                            "pct": {
                                "$percentile": {
                                    "input": "$x",
                                    "p": [0.1, 0.5, 0.75, 0.9],
                                    "method": "approximate",
                                }
                            },
                        }
                    }
                ]
            )
        )[0]
        assert r["med"] == 20.0
        assert r["pct"] == [10.0, 20.0, 30.0, 40.0]

        r = list(
            coll.aggregate(
                [
                    {"$limit": 1},
                    {
                        "$project": {
                            "m": {"$median": {"input": [1, 2, 3, 4], "method": "approximate"}}
                        }
                    },
                ]
            )
        )[0]
        assert r["m"] == 2.0
    finally:
        srv.stop()


def test_expanded_events_match_mongod_shapes(tmp_path) -> None:
    """showExpandedEvents on the Rust server, matching the Python server and a
    mongod 7.0.12 probe: createIndexes / dropIndexes events carry the full
    index description (dropIndexes previously wasn't emitted at all on the
    dropIndexes-"*" path), and expanded update events always carry
    ``disambiguatedPaths`` while unexpanded ones never do."""
    import time

    srv = _server.RustServer(str(tmp_path / "wt"), 0, host="127.0.0.1", replica_set_name="secantus")
    try:
        db = _client(srv)["csx"]
        db.c.insert_one({"_id": 0})
        cs = db.c.watch(show_expanded_events=True, max_await_time_ms=300)
        db.c.create_index([("x", 1)])
        db.c.update_one({"_id": 0}, {"$set": {"a-c": 2}})
        db.c.drop_indexes()
        events = []
        deadline = time.time() + 15
        while time.time() < deadline and len(events) < 3:
            ev = cs.try_next()
            if ev is not None:
                events.append(ev)
        cs.close()
        assert [e["operationType"] for e in events] == [
            "createIndexes",
            "update",
            "dropIndexes",
        ]
        assert events[0]["operationDescription"] == {
            "indexes": [{"v": 2, "key": {"x": 1}, "name": "x_1"}]
        }
        assert events[1]["updateDescription"]["disambiguatedPaths"] == {}
        assert events[2]["operationDescription"] == {
            "indexes": [{"v": 2, "key": {"x": 1}, "name": "x_1"}]
        }

        cs = db.c.watch(max_await_time_ms=300)
        db.c.update_one({"_id": 0}, {"$set": {"n": 1}})
        plain = None
        deadline = time.time() + 15
        while time.time() < deadline and plain is None:
            plain = cs.try_next()
        cs.close()
        assert plain["operationType"] == "update"
        assert "disambiguatedPaths" not in plain["updateDescription"]
    finally:
        srv.stop()


def test_tls_and_x509_auth_end_to_end(tmp_path) -> None:
    """R4 tail: server-side TLS, mTLS client-cert verification, and
    MONGODB-X509 auth (peer_cert_dn threading) on the Rust server — the same
    two-stage bootstrap flow as the Python server's ``test_x509_auth``:
    provision the DN user over plain TLS, restart with ``require_auth`` +
    ``tls_require_client_cert``, authenticate with the cert alone."""
    import ssl

    trustme = pytest.importorskip("trustme")
    from secantus.auth import subject_dn_from_peercert

    ca = trustme.CA()
    server_cert = ca.issue_cert("127.0.0.1")
    cert_path = tmp_path / "server.crt"
    key_path = tmp_path / "server.key"
    ca_path = tmp_path / "ca.crt"
    server_cert.cert_chain_pems[0].write_to_path(cert_path)
    server_cert.private_key_pem.write_to_path(key_path)
    ca.cert_pem.write_to_path(ca_path)

    alice = ca.issue_cert("alice.example", common_name="alice")
    alice_pem = tmp_path / "alice.pem"
    with alice_pem.open("wb") as f:
        for blob in alice.cert_chain_pems:
            f.write(blob.bytes())
        f.write(alice.private_key_pem.bytes())
    cert_only = tmp_path / "alice-cert-only.pem"
    cert_only.write_bytes(alice.cert_chain_pems[0].bytes())
    alice_dn = subject_dn_from_peercert(
        ssl._ssl._test_decode_cert(str(cert_only))  # type: ignore[attr-defined]
    )

    data = str(tmp_path / "wt")
    # Stage 1: TLS-only bootstrap to provision the X509 user.
    srv = _server.RustServer(
        data,
        0,
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
    )
    try:
        host, port = srv.address
        boot = pymongo.MongoClient(
            f"mongodb://{host}:{port}/?tls=true&tlsCAFile={ca_path}",
            serverSelectionTimeoutMS=5000,
        )
        boot["$external"].command(
            "createUser",
            alice_dn,
            roles=[{"role": "root", "db": "admin"}],
            mechanisms=["MONGODB-X509"],
        )
        boot.close()
    finally:
        srv.stop()

    # Stage 2: auth + mTLS on; the cert IS the credential.
    srv = _server.RustServer(
        data,
        0,
        require_auth=True,
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
        tls_ca_file=str(ca_path),
        tls_require_client_cert=True,
    )
    try:
        host, port = srv.address
        client = pymongo.MongoClient(
            f"mongodb://{host}:{port}/?tls=true&tlsCAFile={ca_path}"
            f"&tlsCertificateKeyFile={alice_pem}"
            "&authMechanism=MONGODB-X509&authSource=$external",
            serverSelectionTimeoutMS=5000,
        )
        client["x509db"]["c"].insert_one({"_id": 1, "v": "hello from alice"})
        assert client["x509db"]["c"].find_one({"_id": 1})["v"] == "hello from alice"
        client.close()

        # Negative: a client presenting no cert is refused at the TLS layer.
        bad = pymongo.MongoClient(
            f"mongodb://{host}:{port}/?tls=true&tlsCAFile={ca_path}",
            serverSelectionTimeoutMS=2000,
        )
        with pytest.raises(pymongo.errors.PyMongoError):
            bad.admin.command("ping")
        bad.close()
    finally:
        srv.stop()


def test_tailable_cursor_on_capped_collection(tmp_path) -> None:
    """A tailable cursor on a capped collection drains the initial batch, stays
    open, and picks up documents inserted after the drain — the classic
    tail-a-log pattern (mirrors ``commands.py::_find_tailable``)."""
    import time

    from pymongo.cursor import CursorType

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        db = _client(srv)["t"]
        db.create_collection("log", capped=True, size=65536)
        db.log.insert_many([{"_id": i} for i in range(3)])
        cur = db.log.find(cursor_type=CursorType.TAILABLE)
        got = [d["_id"] for d in cur]
        assert got == [0, 1, 2]
        db.log.insert_one({"_id": 99})
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                got.append(next(cur)["_id"])
                break
            except StopIteration:
                time.sleep(0.1)
        assert got == [0, 1, 2, 99]
        cur.close()
    finally:
        srv.stop()


def test_tailable_capped_follows_inserts_with_nonmonotonic_ids(tmp_path) -> None:
    """A tailable cursor on a capped collection with NON-MONOTONIC custom ``_id``s
    must still surface documents inserted after the drain — in insertion order,
    like mongod (RecordId step 3).

    Regression guard for the bug step 1 introduced: the tailable producer tracked
    position by ``id_key`` (``_id`` sort order), so a follow-up insert carrying a
    ``_id`` SMALLER than the last one already returned sorted *before* the
    watermark and was silently dropped from the stream. Here every follow-up
    ``_id`` is smaller than the initial batch's, so the pre-fix producer would
    hand back nothing; the RecordId-anchored producer follows insertion order and
    returns them. mongod behaves the same — tailable follows insertion order, not
    ``_id``."""
    import time

    from pymongo.cursor import CursorType

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        db = _client(srv)["t"]
        db.create_collection("log", capped=True, size=65536)
        # Initial batch: LARGE _ids.
        db.log.insert_many([{"_id": i} for i in (500, 400, 300)])
        cur = db.log.find(cursor_type=CursorType.TAILABLE)
        got = [d["_id"] for d in cur]
        assert got == [500, 400, 300], "initial batch in insertion order"
        # Follow-up inserts with SMALLER _ids than anything drained — an id_key
        # watermark would exclude all of these.
        for new_id in (20, 10):
            db.log.insert_one({"_id": new_id})
        deadline = time.time() + 10
        while len(got) < 5 and time.time() < deadline:
            try:
                got.append(next(cur)["_id"])
            except StopIteration:
                time.sleep(0.1)
        assert got == [500, 400, 300, 20, 10], (
            f"tailable must follow insertion order, not _id order; got {got}"
        )
        cur.close()
    finally:
        srv.stop()


def test_inc_mul_reject_non_numeric_operand(tmp_path) -> None:
    """$inc / $mul by a non-number (bool / string / null) is REJECTED on the
    Rust server rather than silently computed (bool previously computed as
    5 + 1 = 6). The Rust server surfaces BadValue (code 2) — the documented
    update error-code gap; the Python server gives mongod's exact code 14 —
    but the correctness contract (reject, don't compute) holds on both."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "n": 5})
        for op in ("$inc", "$mul"):
            for operand in (True, "x", None):
                with pytest.raises(pymongo.errors.OperationFailure):
                    coll.update_one({"_id": 1}, {op: {"n": operand}})
        # Untouched by the rejected updates; a valid $inc still applies.
        assert coll.find_one({"_id": 1})["n"] == 5
        coll.update_one({"_id": 1}, {"$inc": {"n": 3}})
        assert coll.find_one({"_id": 1})["n"] == 8
    finally:
        srv.stop()


def test_update_bool_argument_cluster_rejected(tmp_path) -> None:
    """A bool argument to $pop / $push $position / $push $slice / $bit is
    rejected on the Rust server rather than silently treated as 1 (each
    previously either computed or the Rust server errored inconsistently). The
    Rust server surfaces BadValue (the update error-code gap); the correctness
    contract — reject, don't compute — holds. Valid arguments still apply."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": [1, 2, 3]})
        for upd in (
            {"$pop": {"a": True}},
            {"$pop": {"a": 2}},
            {"$push": {"a": {"$each": [9], "$position": True}}},
            {"$push": {"a": {"$each": [], "$slice": True}}},
            {"$bit": {"a": {"and": True}}},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                coll.update_one({"_id": 1}, upd)
        assert coll.find_one({"_id": 1})["a"] == [1, 2, 3]  # untouched
        coll.update_one({"_id": 1}, {"$pop": {"a": 1}})
        assert coll.find_one({"_id": 1})["a"] == [1, 2]
    finally:
        srv.stop()


def test_aggregation_expr_bool_argument_rejected(tmp_path) -> None:
    """A bool where an aggregation expression expects a numeric argument is
    rejected on the Rust server (the core defers → BadValue) rather than
    computing a wrong value. Matches mongod's reject-don't-compute contract; the
    Rust error-code gap means BadValue rather than mongod's per-op Location code.
    A valid int argument still computes."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        for expr in (
            {"$round": [1.5, True]},
            {"$trunc": [1.5, True]},
            {"$arrayElemAt": [[10, 20, 30], True]},
            {"$slice": [[1, 2, 3, 4], True]},
            {"$slice": [[1, 2, 3, 4], 1, True]},
            {"$sortArray": {"input": [3, 1, 2], "sortBy": True}},
            {"$substrCP": ["hello", True, 2]},
            {"$substrBytes": ["hello", True, 2]},
            {"$substr": ["hello", True, 2]},
            {"$range": [0, True]},
            {"$indexOfArray": [[1, 2, 3], 2, True]},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        out = list(
            coll.aggregate([{"$project": {"r": {"$arrayElemAt": [[10, 20, 30], 1]}, "_id": 0}}])
        )
        assert out == [{"r": 20}]
    finally:
        srv.stop()


def test_aggregation_whole_double_index_accepted(tmp_path) -> None:
    """The Rust server accepts a whole-number double index (computes, matching
    the Python server) and rejects a fractional double. Prevents the two servers
    diverging on a valid whole-double index."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        got = list(
            coll.aggregate([{"$project": {"r": {"$arrayElemAt": [[10, 20, 30], 2.0]}, "_id": 0}}])
        )
        assert got == [{"r": 30}]
        got = list(
            coll.aggregate([{"$project": {"r": {"$slice": [[1, 2, 3, 4], 1.0, 2.0]}, "_id": 0}}])
        )
        assert got == [{"r": [2, 3]}]
        # whole-double computes on the Rust server too (parity with Python).
        assert list(
            coll.aggregate([{"$project": {"r": {"$range": [0.0, 5.0, 1.0]}, "_id": 0}}])
        ) == [{"r": [0, 1, 2, 3, 4]}]
        for expr in (
            {"$arrayElemAt": [[10, 20, 30], 2.7]},
            {"$slice": [[1, 2, 3, 4], 2.7]},
            {"$indexOfArray": [[1, 2, 3], 2, 0.7]},
            {"$substrCP": ["hello", 1.7, 2]},
            {"$range": [0, 5.7]},
            {"$round": [3.14159, 2.7]},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
    finally:
        srv.stop()


def test_substr_bytes_split_utf8_rejected(tmp_path) -> None:
    """The Rust server rejects a $substrBytes range that splits a UTF-8
    character (its byte slice isn't valid UTF-8, so the core defers to BadValue)
    rather than returning a replacement char. Clean boundaries still compute."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        for expr in (
            {"$substrBytes": ["héllo", 0, 2]},
            {"$substrBytes": ["héllo", 2, 3]},
            {"$substr": ["héllo", 0, 2]},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        got = list(
            coll.aggregate([{"$project": {"r": {"$substrBytes": ["héllo", 0, 3]}, "_id": 0}}])
        )
        assert got == [{"r": "hé"}]
    finally:
        srv.stop()


def test_substr_negative_index_rejected(tmp_path) -> None:
    """The Rust server rejects a negative $substr* start / negative $substrCP
    length (the core defers to BadValue), while $substrBytes negative length
    still means "to end"."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        for expr in (
            {"$substrBytes": ["abcde", -1, 2]},
            {"$substrCP": ["abcde", -1, 2]},
            {"$substrCP": ["abcde", 1, -1]},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        got = list(
            coll.aggregate([{"$project": {"r": {"$substrBytes": ["abcde", 1, -1]}, "_id": 0}}])
        )
        assert got == [{"r": "bcde"}]
    finally:
        srv.stop()


def test_substr_bytes_truncates_double_index(tmp_path) -> None:
    """The Rust server truncates a $substrBytes double index toward zero
    (computing, matching the Python server), not defer."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        for expr, want in [
            ({"$substrBytes": ["abcde", 1.7, 2]}, "bc"),
            ({"$substrBytes": ["abcde", 0.9, 3]}, "abc"),
            ({"$substrBytes": ["abcde", 1, 2.9]}, "bc"),
        ]:
            got = list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
            assert got == [{"r": want}], expr
    finally:
        srv.stop()


def test_limit_skip_numeric_arg_validation(tmp_path) -> None:
    """The Rust server accepts a whole-double $limit/$skip (computing, matching
    the Python server) and rejects bool / fractional / negative / zero-$limit
    (the core defers to BadValue)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": i} for i in range(10)])
        assert len(list(coll.aggregate([{"$limit": 2.0}]))) == 2
        assert len(list(coll.aggregate([{"$skip": 3.0}]))) == 7
        for pipe in (
            [{"$limit": 2.7}],
            [{"$limit": True}],
            [{"$limit": 0}],
            [{"$skip": 3.7}],
            [{"$skip": -1}],
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate(pipe))
    finally:
        srv.stop()


def test_sample_size_validation(tmp_path) -> None:
    """The Rust server already rejects a bool / negative $sample size (regression
    guard) and accepts a whole / fractional one."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": i} for i in range(10)])
        assert len(list(coll.aggregate([{"$sample": {"size": 3}}]))) == 3
        assert len(list(coll.aggregate([{"$sample": {"size": 2.7}}]))) == 2
        for size in (True, -1):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$sample": {"size": size}}]))
    finally:
        srv.stop()


def test_bits_numeric_arg_validation(tmp_path) -> None:
    """The Rust server accepts a whole-double $bits* mask/position (computing,
    matching the Python server) and rejects fractional / negative / bool (the
    core defers to BadValue)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "n": 6})
        assert coll.count_documents({"n": {"$bitsAllSet": 6.0}}) == 1
        assert coll.count_documents({"n": {"$bitsAllSet": [1.0, 2.0]}}) == 1
        for query in (
            {"n": {"$bitsAllSet": 2.5}},
            {"n": {"$bitsAllSet": -1}},
            {"n": {"$bitsAllSet": [1.5]}},
            {"n": {"$bitsAllSet": [-1]}},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                coll.count_documents(query)
    finally:
        srv.stop()


def test_pow_domain_validation(tmp_path) -> None:
    """The Rust server returns NaN for a negative base + fractional exponent
    (matching Python, not a crash) and rejects bad operands (defer -> BadValue)."""
    import math

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        got = list(coll.aggregate([{"$project": {"r": {"$pow": [-2, 0.5]}, "_id": 0}}]))
        assert len(got) == 1 and math.isnan(got[0]["r"])
        assert (
            list(coll.aggregate([{"$project": {"r": {"$pow": [-2, 3]}, "_id": 0}}]))[0]["r"] == -8
        )
        for expr in ({"$pow": ["x", 2]}, {"$pow": [2, True]}, {"$pow": [0, -1]}):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
    finally:
        srv.stop()


def test_gte_lte_null_and_exists_truthiness(tmp_path) -> None:
    """The Rust server matches null+missing for $gte/$lte: null and uses mongod
    truthiness for $exists (empty string/array truthy), matching the Python server."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": 1, "f": None}, {"_id": 2, "f": 5}, {"_id": 3}])

        def ids(q):
            return sorted(d["_id"] for d in coll.find(q))

        assert ids({"f": {"$gte": None}}) == [1, 3]
        assert ids({"f": {"$lte": None}}) == [1, 3]
        assert ids({"f": {"$gt": None}}) == []
        assert ids({"f": {"$exists": ""}}) == [1, 2]
        assert ids({"f": {"$exists": []}}) == [1, 2]
        assert ids({"f": {"$exists": 0}}) == [3]
    finally:
        srv.stop()


def test_rename_validation_no_corruption(tmp_path) -> None:
    """The Rust server rejects a corrupting/invalid $rename (defer -> BadValue)
    and leaves the array intact; a valid rename still applies."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": 5, "arr": [1, 2, 3]})
        for upd in (
            {"$rename": {"a": "a"}},
            {"$rename": {"arr.0": "x"}},
            {"$rename": {"a": ""}},
            {"$rename": {"a": 5}},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                coll.update_one({"_id": 1}, upd)
        assert coll.find_one({"_id": 1})["arr"] == [1, 2, 3]
        coll.update_one({"_id": 1}, {"$rename": {"a": "z"}})
        assert coll.find_one({"_id": 1}).get("z") == 5
    finally:
        srv.stop()


def test_bucket_validation_no_data_loss(tmp_path) -> None:
    """The Rust server errors (defer -> BadValue) on an out-of-range $bucket value
    with no default instead of silently dropping the document."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": i, "v": i} for i in range(6)])
        r = list(coll.aggregate([{"$bucket": {"groupBy": "$v", "boundaries": [0, 3, 6]}}]))
        assert [(b["_id"], b["count"]) for b in r] == [(0, 3), (3, 3)]
        for spec in (
            {"groupBy": "$v", "boundaries": [0, 3]},  # out of range, no default
            {"groupBy": "$v", "boundaries": [0, 5, 2]},  # unsorted
            {"boundaries": [0, 6]},  # missing groupBy
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$bucket": spec}]))
    finally:
        srv.stop()


def test_count_project_sort_by_count_stage_validation(tmp_path) -> None:
    """The Rust server errors (defer -> BadValue) on an invalid $count field, an
    empty $project spec, or a non-expression $sortByCount argument instead of
    silently computing a wrong (or corrupt-field-named) result. Valid forms of
    each still run."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 1}, {"_id": 3, "v": 2}])
        for pipeline in (
            [{"$count": 5}],  # non-string
            [{"$count": ""}],  # empty
            [{"$count": "$n"}],  # $-prefixed
            [{"$count": "a.b"}],  # dotted
            [{"$count": "_id"}],  # reserved _id
            [{"$project": {}}],  # empty projection
            [{"$sortByCount": 5}],  # non-expression scalar
            [{"$sortByCount": "v"}],  # bare (non-$) path
            [{"$sortByCount": {"a": 1}}],  # non-$ object
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate(pipeline))
        # Valid forms still run.
        assert list(coll.aggregate([{"$count": "n"}])) == [{"n": 3}]
        assert list(coll.aggregate([{"$project": {"_id": 0, "v": 1}}])) == [
            {"v": 1},
            {"v": 1},
            {"v": 2},
        ]
        assert list(coll.aggregate([{"$sortByCount": "$v"}])) == [
            {"_id": 1, "count": 2},
            {"_id": 2, "count": 1},
        ]
    finally:
        srv.stop()


def test_bucket_auto_elem_match_pull_validation(tmp_path) -> None:
    """The Rust server errors (defer -> BadValue) on an invalid $bucketAuto
    'buckets' argument, a non-document $elemMatch projection argument, or a
    $pull/$pullAll against a present non-array field — instead of silently
    accepting bad input or corrupting/dropping data. Valid forms still run and a
    valid $pull removes the matching element."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": i, "v": i} for i in range(6)])
        for spec in (
            {"groupBy": "$v", "buckets": True},
            {"groupBy": "$v", "buckets": "x"},
            {"groupBy": "$v", "buckets": 2.5},
            {"groupBy": "$v", "buckets": 0},
            {"groupBy": "$v"},
            {"buckets": 2},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$bucketAuto": spec}]))
        # A whole-double buckets is accepted.
        assert len(list(coll.aggregate([{"$bucketAuto": {"groupBy": "$v", "buckets": 2.0}}]))) == 2
        # An invalid `granularity` name errors (defer -> BadValue): a non-string or
        # an unknown series. A valid series now computes (see the dedicated
        # test_bucket_auto_granularity_against_rust_server).
        for gran in (5, "BOGUS"):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(
                    coll.aggregate(
                        [{"$bucketAuto": {"groupBy": "$v", "buckets": 2, "granularity": gran}}]
                    )
                )

        # Non-document $elemMatch projection argument.
        for arg in (5, "x", [1]):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.find({}, {"v": {"$elemMatch": arg}}))

        # $pull / $pullAll against a present non-array field errors, no data loss.
        coll.drop()
        coll.insert_one({"_id": 1, "num": 5, "nul": None, "arr": [1, 2, 3]})
        for upd in (
            {"$pull": {"num": 1}},
            {"$pullAll": {"num": [1]}},
            {"$pull": {"nul": 1}},
            {"$pullAll": {"nul": [1]}},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                coll.update_one({"_id": 1}, upd)
        # The document is untouched by the failed updates.
        assert coll.find_one({"_id": 1}) == {"_id": 1, "num": 5, "nul": None, "arr": [1, 2, 3]}
        # A missing field is a no-op; a valid array pull removes the element.
        coll.update_one({"_id": 1}, {"$pull": {"nope": 1}})
        coll.update_one({"_id": 1}, {"$pull": {"arr": 2}})
        assert coll.find_one({"_id": 1})["arr"] == [1, 3]
    finally:
        srv.stop()


def test_push_sort_and_current_date_validation(tmp_path) -> None:
    """The Rust server rejects an invalid $push $sort direction (defer -> BadValue)
    without corrupting the array, accepts a valid ±1 sort, and — matching the
    Python fix — accepts a boolean-false $currentDate as the set-Date form."""
    import datetime

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": [{"s": 3}, {"s": 1}]})
        # A valid ±1 sort works.
        coll.update_one({"_id": 1}, {"$push": {"a": {"$each": [{"s": 2}], "$sort": 1}}})
        assert [e["s"] for e in coll.find_one({"_id": 1})["a"]] == [1, 2, 3]
        # An invalid sort direction errors and leaves the array untouched.
        for spec in (2, {"s": 2}, "x"):
            with pytest.raises(pymongo.errors.OperationFailure):
                coll.update_one({"_id": 1}, {"$push": {"a": {"$each": [{"s": 9}], "$sort": spec}}})
        assert [e["s"] for e in coll.find_one({"_id": 1})["a"]] == [1, 2, 3]

        # $currentDate: a boolean (true OR false) sets the current Date.
        coll.update_one({"_id": 1}, {"$currentDate": {"d": True}})
        assert isinstance(coll.find_one({"_id": 1})["d"], datetime.datetime)
        coll.update_one({"_id": 1}, {"$currentDate": {"d": False}})
        assert isinstance(coll.find_one({"_id": 1})["d"], datetime.datetime)
    finally:
        srv.stop()


def test_array_filters_validation(tmp_path) -> None:
    """The Rust server rejects invalid arrayFilters (defer -> BadValue) without
    touching the document, and still applies a valid filter to the matching
    array elements."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": [{"g": 1}, {"g": 5}]})
        upd = {"$set": {"a.$[x].g": 9}}
        for af in (
            [{}],  # empty filter
            [{"1x": {"$gt": 0}}],  # bad identifier
            [{"x": {"$gt": 0}}, {"x": {"$lt": 9}}],  # duplicate identifier
            [{"x": {"$gt": 0}}, {"y": {"$gt": 0}}],  # 'y' unused
            [{"$and": [{"x": {"$gt": 0}}, {"y": {"$gt": 0}}]}],  # two idents nested
            [{"$expr": {"$gt": ["$g", 0]}}],  # $expr, no identifier
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                coll.update_one({"_id": 1}, upd, array_filters=af)
        # The document is untouched by the failed updates.
        assert coll.find_one({"_id": 1})["a"] == [{"g": 1}, {"g": 5}]
        # A valid filter updates only the matching element.
        coll.update_one({"_id": 1}, upd, array_filters=[{"x.g": {"$gt": 3}}])
        assert [e["g"] for e in coll.find_one({"_id": 1})["a"]] == [1, 9]
        # A single identifier nested inside $and resolves and applies.
        coll.update_one(
            {"_id": 1}, {"$set": {"a.$[x].g": 7}}, array_filters=[{"$and": [{"x.g": {"$gt": 8}}]}]
        )
        assert [e["g"] for e in coll.find_one({"_id": 1})["a"]] == [1, 7]
    finally:
        srv.stop()


def test_to_int_convert_overflow(tmp_path) -> None:
    """The Rust server rejects an int32/int64 overflow in $toInt / $convert
    (defer -> BadValue) and downcasts an in-range long to int32; $convert
    onError still catches the overflow."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "big": 3_000_000_000.0, "small": bson.Int64(5)})
        with pytest.raises(pymongo.errors.OperationFailure):
            list(coll.aggregate([{"$project": {"n": {"$toInt": "$big"}}}]))
        with pytest.raises(pymongo.errors.OperationFailure):
            list(
                coll.aggregate([{"$project": {"n": {"$convert": {"input": "$big", "to": "int"}}}}])
            )
        # in-range long -> int32
        r = list(coll.aggregate([{"$project": {"_id": 0, "n": {"$toInt": "$small"}}}]))
        assert r == [{"n": 5}]
        # onError catches the overflow
        r = list(
            coll.aggregate(
                [
                    {
                        "$project": {
                            "_id": 0,
                            "n": {"$convert": {"input": "$big", "to": "int", "onError": -1}},
                        }
                    }
                ]
            )
        )
        assert r == [{"n": -1}]
    finally:
        srv.stop()


def test_date_misc_typeguard_validation(tmp_path) -> None:
    """The Rust server errors (defer -> BadValue) on $dateToString with a non-date
    and $dateDiff with a missing endDate — both previously silent accepts — and on
    an unknown $dateTrunc unit. A valid $dateToString still computes."""
    import datetime

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "n": 5, "d": datetime.datetime(2020, 1, 1)})
        for expr in (
            {"$dateToString": {"date": "$n"}},  # was a silent null
            {"$dateDiff": {"startDate": "$d"}},  # missing endDate, was a silent null
            {"$dateTrunc": {"date": "$d", "unit": "bogus"}},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"v": expr}}]))
        # A valid $dateToString still computes.
        got = list(
            coll.aggregate([{"$project": {"_id": 0, "s": {"$dateToString": {"date": "$d"}}}}])
        )
        assert got == [{"s": "2020-01-01T00:00:00.000Z"}]
    finally:
        srv.stop()


def test_array_set_typeguard_validation(tmp_path) -> None:
    """The Rust server errors (defer -> BadValue) on a non-array/non-object
    argument to array/set operators — including $in and $arrayElemAt, which
    previously silently returned a value — instead of computing a wrong result.
    Valid forms still compute."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "n": 5})
        for expr in (
            {"$size": "$n"},
            {"$arrayElemAt": ["$n", 0]},
            {"$in": [1, "$n"]},
            {"$setUnion": ["$n"]},
            {"$mergeObjects": ["$n"]},
            {"$regexMatch": {"input": "$n", "regex": "a"}},  # was a silent accept
            {"$regexFindAll": {"input": "$n", "regex": "a"}},  # was a silent accept
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"v": expr}}]))
        # Valid forms still compute (incl. a null regex input -> false).
        got = list(
            coll.aggregate(
                [
                    {
                        "$project": {
                            "_id": 0,
                            "s": {"$size": [1, 2, 3]},
                            "i": {"$in": [2, [1, 2]]},
                            "r": {"$regexMatch": {"input": "abc", "regex": "b"}},
                        }
                    }
                ]
            )
        )
        assert got == [{"s": 3, "i": True, "r": True}]
    finally:
        srv.stop()


def test_expression_accumulators(tmp_path) -> None:
    """The Rust server computes $sum/$avg/$max/$min as expression operators
    (MongoDB 5.0+) — over an array or a scalar — with the same numeric widths and
    cross-type ordering as the Python server."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "arr": [3, 1, 2], "n": 5})
        got = list(
            coll.aggregate(
                [
                    {
                        "$project": {
                            "_id": 0,
                            "s": {"$sum": "$arr"},
                            "a": {"$avg": "$arr"},
                            "mx": {"$max": "$arr"},
                            "mn": {"$min": "$arr"},
                            "sn": {"$sum": "$n"},
                            "se": {"$sum": []},  # empty -> 0
                            "ae": {"$avg": []},  # empty -> null
                        }
                    }
                ]
            )
        )
        assert got == [{"s": 6, "a": 2.0, "mx": 3, "mn": 1, "sn": 5, "se": 0, "ae": None}]
    finally:
        srv.stop()


def test_strcasecmp_coercion(tmp_path) -> None:
    """The Rust server $toString-coerces integer $strcasecmp operands (matching
    mongod / the Python server) instead of erroring."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "n": 5})
        got = list(
            coll.aggregate(
                [
                    {
                        "$project": {
                            "_id": 0,
                            "a": {"$strcasecmp": ["$n", "a"]},
                            "b": {"$strcasecmp": [5, 10]},
                        }
                    }
                ]
            )
        )
        assert got == [{"a": -1, "b": 1}]
    finally:
        srv.stop()


def test_to_long_conversion(tmp_path) -> None:
    """The Rust server computes $toLong (truncating toward zero, yielding a 64-bit
    long that can exceed int32) and rejects an int64 overflow (defer -> BadValue)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "d": 2.7, "big": 9_000_000_000.0})
        r = list(
            coll.aggregate(
                [{"$project": {"_id": 0, "a": {"$toLong": "$d"}, "b": {"$toLong": "$big"}}}]
            )
        )
        assert r == [{"a": 2, "b": 9_000_000_000}]
        assert all(isinstance(r[0][k], bson.Int64) for k in ("a", "b"))
        # Overflow beyond int64 errors.
        coll.update_one({"_id": 1}, {"$set": {"huge": "99999999999999999999"}})
        with pytest.raises(pymongo.errors.OperationFailure):
            list(coll.aggregate([{"$project": {"n": {"$toLong": "$huge"}}}]))
    finally:
        srv.stop()


def test_group_accumulator_mixed_types(tmp_path) -> None:
    """The Rust server's $group ignores non-numeric in $sum/$avg and orders
    $min/$max by BSON cross-type (bool > string > number), matching mongod —
    no longer deferring/erroring on a mixed-type group."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many(
            [
                {"_id": 1, "v": 10},
                {"_id": 2, "v": "hi"},
                {"_id": 3, "v": True},
                {"_id": 4, "v": None},
                {"_id": 5},
                {"_id": 6, "v": 2.5},
                {"_id": 7, "v": bson.Int64(3)},
            ]
        )
        [b] = list(
            coll.aggregate(
                [
                    {
                        "$group": {
                            "_id": None,
                            "s": {"$sum": "$v"},
                            "a": {"$avg": "$v"},
                            "mn": {"$min": "$v"},
                            "mx": {"$max": "$v"},
                        }
                    }
                ]
            )
        )
        assert b["s"] == 15.5
        assert b["a"] == 15.5 / 3
        assert b["mn"] == 2.5
        assert b["mx"] is True
    finally:
        srv.stop()


def test_unary_math_rejects_non_numeric(tmp_path) -> None:
    """The Rust server rejects a string/bool operand to the unary math ops
    (defer -> BadValue) instead of coercing a bool or erroring internally; a
    whole-double operand still computes."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "s": "x", "n": 4.0})
        for op in ("$abs", "$ceil", "$floor", "$sqrt", "$exp", "$ln", "$log10", "$trunc", "$round"):
            arg = ["$s", 0] if op in ("$trunc", "$round") else "$s"
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": {op: arg}}}]))
            with pytest.raises(pymongo.errors.OperationFailure):
                bad = [True, 0] if op in ("$trunc", "$round") else True
                list(coll.aggregate([{"$project": {"r": {op: bad}}}]))
        # $log rejects a non-numeric argument / base too.
        for expr in ({"$log": ["$s", 2]}, {"$log": [8, "$s"]}, {"$log": [True, 2]}):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": expr}}]))
        got = list(coll.aggregate([{"$project": {"_id": 0, "a": {"$abs": "$n"}}}]))
        assert got == [{"a": 4.0}]
    finally:
        srv.stop()


def test_in_nin_argument_validation(tmp_path) -> None:
    """The Rust server rejects a non-array $in/$nin and a nested $-prefixed doc
    element (defer -> BadValue) instead of leaking / silently no-matching."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "n": 5})
        for q in ({"n": {"$in": 5}}, {"n": {"$nin": "x"}}, {"n": {"$in": [{"$x": 1}]}}):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.find(q))
        assert [d["_id"] for d in coll.find({"n": {"$in": [5, 9]}})] == [1]
    finally:
        srv.stop()


def test_regex_options_validation(tmp_path) -> None:
    """The Rust server rejects a bad $regex flag / non-string $options /
    $options-without-$regex / non-string $regex (defer -> BadValue) instead of
    silently ignoring; a valid case-insensitive regex still matches."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "s": "Hello"})
        for q in (
            {"s": {"$regex": "h", "$options": "z"}},
            {"s": {"$regex": "h", "$options": 5}},
            {"s": {"$options": "i"}},
            {"s": {"$regex": 5}},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.find(q))
        assert [d["_id"] for d in coll.find({"s": {"$regex": "^h", "$options": "i"}})] == [1]
    finally:
        srv.stop()


def test_not_elemmatch_validation(tmp_path) -> None:
    """The Rust server rejects an invalid $not (non-regex/doc, empty doc) and a
    non-object $elemMatch (defer -> BadValue); valid forms still match."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": 5, "arr": [1, 2, 3]})
        for q in (
            {"a": {"$not": 5}},
            {"a": {"$not": {}}},
            {"a": {"$not": True}},
            {"arr": {"$elemMatch": 5}},
            {"arr": {"$elemMatch": "x"}},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.find(q))
        assert [d["_id"] for d in coll.find({"a": {"$not": {"$gt": 9}}})] == [1]
        assert [d["_id"] for d in coll.find({"arr": {"$elemMatch": {"$gt": 2}}})] == [1]
    finally:
        srv.stop()


def test_all_argument_validation(tmp_path) -> None:
    """The Rust server rejects a non-array $all and a mixed/non-$elemMatch
    $-expression element (defer -> BadValue); valid forms still match."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": [1, 2, 3]})
        for q in (
            {"a": {"$all": 5}},
            {"a": {"$all": [1, {"$elemMatch": {"x": 1}}]}},
            {"a": {"$all": [{"$gt": 1}]}},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.find(q))
        assert [d["_id"] for d in coll.find({"a": {"$all": [1, 2]}})] == [1]
        assert [d["_id"] for d in coll.find({"a": {"$all": [{"$elemMatch": {"$gt": 2}}]}})] == [1]
    finally:
        srv.stop()


def test_split_argument_validation(tmp_path) -> None:
    """The Rust server rejects an invalid $split (empty separator, non-string
    operand, wrong arg count) instead of leaking; valid $split still computes and
    a null operand yields null."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        for expr in ({"$split": ["a,b", ""]}, {"$split": [5, ","]}, {"$split": ["a,b"]}):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        got = list(
            coll.aggregate(
                [
                    {
                        "$project": {
                            "_id": 0,
                            "a": {"$split": ["a,b,c", ","]},
                            "n": {"$split": [None, ","]},
                        }
                    }
                ]
            )
        )
        assert got == [{"a": ["a", "b", "c"], "n": None}]
    finally:
        srv.stop()


def test_sort_stage_validation(tmp_path) -> None:
    """The Rust server rejects an invalid $sort stage (non-numeric / non-±1
    direction, empty spec) instead of coercing a bool or leaking; a whole-double
    direction still sorts."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": 1, "n": 3}, {"_id": 2, "n": 1}])
        for spec in ({"n": "asc"}, {"n": True}, {"n": 0}, {"n": 2}, {}):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$sort": spec}]))
        assert [d["_id"] for d in coll.aggregate([{"$sort": {"n": 1.0}}])] == [2, 1]
    finally:
        srv.stop()


def test_densify_validation(tmp_path) -> None:
    """The Rust server rejects an invalid $densify (date unit on numeric, bool
    step, non-positive step, bad/wrong-length/descending bounds) instead of
    coercing or leaking; a valid numeric densify still fills the gap."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 5}])
        for rng in (
            {"step": 1, "unit": "day", "bounds": "full"},
            {"step": True, "bounds": "full"},
            {"step": 0, "bounds": "full"},
            {"step": 1, "bounds": "partial"},
            {"step": 1, "bounds": [0]},
            {"step": 1, "bounds": [5, 0]},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$densify": {"field": "v", "range": rng}}]))
        out = list(
            coll.aggregate([{"$densify": {"field": "v", "range": {"step": 1, "bounds": "full"}}}])
        )
        assert sorted(d["v"] for d in out) == [1, 2, 3, 4, 5]
    finally:
        srv.stop()


def test_facet_validation(tmp_path) -> None:
    """The Rust server rejects an invalid $facet (empty spec, non-array
    sub-pipeline, non-object stage, nested $facet) instead of leaking; a valid
    $facet still runs its sub-pipelines."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
        for spec in (
            {},
            {"a": 5},
            {"a": [5]},
            {"a": [{"$facet": {"b": [{"$match": {"v": 1}}]}}]},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$facet": spec}]))
        out = list(coll.aggregate([{"$facet": {"n": [{"$count": "c"}]}}]))
        assert out == [{"n": [{"c": 2}]}]
    finally:
        srv.stop()


def test_projection_slice_validation(tmp_path) -> None:
    """The Rust server rejects an invalid projection $slice (non-number scalar,
    empty/bad array) instead of silently returning the array; a valid $slice
    still reshapes it."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": [1, 2, 3, 4, 5]})
        for sl in ("x", [], [1, -2], [1, 2, 3], ["x", 2]):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.find({}, {"a": {"$slice": sl}}))
        assert coll.find_one({}, {"_id": 0, "a": {"$slice": 2}})["a"] == [1, 2]
        assert coll.find_one({}, {"_id": 0, "a": {"$slice": [1, 2]}})["a"] == [2, 3]
    finally:
        srv.stop()


def test_type_argument_validation(tmp_path) -> None:
    """The Rust server rejects an invalid $type (unknown alias, out-of-range /
    fractional code, bool) instead of silently no-matching, and accepts a valid
    alias / numeric / whole-double code."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": 5})
        for t in ("notatype", 0, 100, 2.5, True):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.find({"a": {"$type": t}}))
        assert [d["_id"] for d in coll.find({"a": {"$type": "int"}})] == [1]
        assert [d["_id"] for d in coll.find({"a": {"$type": 16.0}})] == [1]
    finally:
        srv.stop()


def test_concat_type_validation(tmp_path) -> None:
    """The Rust server rejects a non-string $concat operand (defer -> BadValue)
    instead of coercing it; a null / missing operand yields null."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "s": "b"})
        for expr in ({"$concat": ["a", 5]}, {"$concat": ["a", True]}):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        proj = {"_id": 0, "ok": {"$concat": ["a", "$s"]}, "n": {"$concat": ["a", None]}}
        out = list(coll.aggregate([{"$project": proj}]))
        assert out == [{"ok": "ab", "n": None}]
    finally:
        srv.stop()


def test_array_operators_reject_non_array(tmp_path) -> None:
    """The Rust server rejects a non-array input to the array operators (defer ->
    BadValue) instead of silently yielding null; a null / missing input still
    yields null."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        for expr in (
            {"$first": 5},
            {"$reverseArray": 5},
            {"$concatArrays": [[1], 5]},
            {"$map": {"input": 5, "in": "$$this"}},
            {"$filter": {"input": 5, "cond": True}},
            {"$reduce": {"input": 5, "initialValue": 0, "in": "$$value"}},
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": expr, "_id": 0}}]))
        out = list(coll.aggregate([{"$project": {"_id": 0, "n": {"$first": "$gone"}}}]))
        assert out == [{"n": None}]
    finally:
        srv.stop()


def test_trim_argument_validation(tmp_path) -> None:
    """The Rust server rejects a non-string $trim input / chars (defer ->
    BadValue) and returns null for a null chars; a valid chars still trims."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        for op in ("$trim", "$ltrim", "$rtrim"):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$project": {"r": {op: {"input": 5}}, "_id": 0}}]))
            with pytest.raises(pymongo.errors.OperationFailure):
                list(
                    coll.aggregate(
                        [{"$project": {"r": {op: {"input": "x", "chars": 5}}, "_id": 0}}]
                    )
                )
        proj = {
            "_id": 0,
            "t": {"$trim": {"input": "--x--", "chars": "-"}},
            "n": {"$trim": {"input": "x", "chars": None}},
        }
        assert list(coll.aggregate([{"$project": proj}])) == [{"t": "x", "n": None}]
    finally:
        srv.stop()


def test_index_of_start_end_validation(tmp_path) -> None:
    """The Rust server rejects a fractional / bool / non-numeric / negative
    $indexOf start or end (defer -> BadValue) and accepts a whole double."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        for op in ("$indexOfBytes", "$indexOfCP"):
            for bad in (2.5, True, "x", -1):
                with pytest.raises(pymongo.errors.OperationFailure):
                    list(
                        coll.aggregate([{"$project": {"r": {op: ["abcabc", "b", bad]}, "_id": 0}}])
                    )
        proj = {"_id": 0, "i": {"$indexOfBytes": ["abcabc", "b", 2.0]}}
        assert list(coll.aggregate([{"$project": proj}])) == [{"i": 4}]
    finally:
        srv.stop()


def test_to_date_rejects_bool(tmp_path) -> None:
    """The Rust server rejects $toDate on a bool (defer -> BadValue) instead of
    coercing it to a date; $convert onError still catches it."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1})
        with pytest.raises(pymongo.errors.OperationFailure):
            list(coll.aggregate([{"$project": {"r": {"$toDate": True}, "_id": 0}}]))
        proj = {"_id": 0, "r": {"$convert": {"input": True, "to": "date", "onError": "x"}}}
        assert list(coll.aggregate([{"$project": proj}])) == [{"r": "x"}]
    finally:
        srv.stop()


def test_date_arg_validation(tmp_path) -> None:
    """The Rust server rejects a fractional / bool $dateAdd amount / $dateTrunc
    binSize (defer -> BadValue) and computes a whole-double one."""
    import datetime as _dt

    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "d": _dt.datetime(2021, 1, 1)})
        for op, key, sub in (
            ("$dateAdd", "amount", "startDate"),
            ("$dateSubtract", "amount", "startDate"),
            ("$dateTrunc", "binSize", "date"),
        ):
            for bad in (2.5, True):
                spec = {sub: "$d", "unit": "day", key: bad}
                with pytest.raises(pymongo.errors.OperationFailure):
                    list(coll.aggregate([{"$project": {"r": {op: spec}}}]))
        proj = {"_id": 0, "r": {"$dateAdd": {"startDate": "$d", "unit": "day", "amount": 2.0}}}
        assert list(coll.aggregate([{"$project": proj}])) == [{"r": _dt.datetime(2021, 1, 3)}]
    finally:
        srv.stop()


def test_unwind_validation(tmp_path) -> None:
    """The Rust server rejects an invalid $unwind (bare path, non-string / empty /
    $-prefixed includeArrayIndex, non-bool preserve) instead of computing; a valid
    $unwind still expands the array."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_one({"_id": 1, "a": [1, 2, 3]})
        for spec in (
            {"path": "a"},
            {"path": 5},
            {"path": "$a", "includeArrayIndex": 5},
            {"path": "$a", "includeArrayIndex": ""},
            {"path": "$a", "includeArrayIndex": "$i"},
            {"path": "$a", "preserveNullAndEmptyArrays": 5},
            "a",
        ):
            with pytest.raises(pymongo.errors.OperationFailure):
                list(coll.aggregate([{"$unwind": spec}]))
        assert len(list(coll.aggregate([{"$unwind": "$a"}]))) == 3
    finally:
        srv.stop()


def test_bucket_auto_granularity_against_rust_server(tmp_path) -> None:
    """$bucketAuto `granularity` preferred-number rounding on the Rust server:
    exact boundaries (incl. mongod's ULP 63*0.1 = 6.300000000000001) and the
    non-negative-number value errors (40258/40259/40260)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": i, "v": i + 1} for i in range(8)])  # v = 1..8
        out = list(
            coll.aggregate([{"$bucketAuto": {"groupBy": "$v", "buckets": 2, "granularity": "R5"}}])
        )
        assert [(b["_id"]["min"], b["_id"]["max"], b["count"]) for b in out] == [
            (0.63, 6.300000000000001, 6),
            (6.300000000000001, 10.0, 2),
        ]
        out = list(
            coll.aggregate(
                [{"$bucketAuto": {"groupBy": "$v", "buckets": 2, "granularity": "POWERSOF2"}}]
            )
        )
        assert [(b["_id"]["min"], b["_id"]["max"]) for b in out] == [(0.5, 8.0), (8.0, 16.0)]

        # negative value -> error (the Rust server renders BadValue; correctness =
        # it rejects rather than computing a wrong boundary).
        coll.delete_many({})
        coll.insert_many([{"_id": 0, "v": -1.0}, {"_id": 1, "v": 2.0}])
        with pytest.raises(pymongo.errors.OperationFailure):
            list(
                coll.aggregate(
                    [{"$bucketAuto": {"groupBy": "$v", "buckets": 2, "granularity": "R5"}}]
                )
            )
    finally:
        srv.stop()


def test_multikey_dotted_array_index_against_rust_server(tmp_path) -> None:
    """An index on a dotted path *into* an array of subdocuments must return
    the enclosing document from an index scan, report `isMultiKey` in explain,
    and keep the internal flag off `listIndexes` — same contract the Python
    server holds (tests/test_multikey_index_{find,metadata}.py), probed against
    mongod 6.0.16.
    """
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["wine_prices"]
        owner = bson.ObjectId()
        coll.insert_one(
            {"prices": [{"owner_id": bson.ObjectId()}, {"owner_id": owner, "price": 10.0}]}
        )
        coll.create_index([("prices.owner_id", 1)], name="prices_owner_id")

        assert len(list(coll.find({"prices.owner_id": owner}))) == 1
        assert len(list(coll.find({"prices.owner_id": {"$in": [owner]}}))) == 1

        ix = next(i for i in coll.list_indexes() if i["name"] == "prices_owner_id")
        assert dict(ix) == {
            "v": 2,
            "key": {"prices.owner_id": 1},
            "name": "prices_owner_id",
        }

        winning = coll.find({"prices.owner_id": owner}).explain()["queryPlanner"]["winningPlan"]
        ixscan = winning if winning["stage"] == "IXSCAN" else winning.get("inputStage", {})
        assert ixscan.get("stage") == "IXSCAN"
        assert ixscan.get("isMultiKey") is True
    finally:
        srv.stop()


def test_storage_mode_kwargs_end_to_end(tmp_path, monkeypatch) -> None:
    """Phase C: the async/nonlogged stack engages from RustServer kwargs alone —
    no SECANTUS_* env vars — and stays pymongo-clean end-to-end (writes, reads,
    and a change stream whose events surface once the drainer persists them)."""
    for var in ("SECANTUS_OPLOG_ASYNC", "SECANTUS_OPLOG_NONLOGGED"):
        monkeypatch.delenv(var, raising=False)
    srv = _server.RustServer(
        str(tmp_path / "wt"),
        0,
        replica_set_name="secantus",  # change streams need the replset persona
        oplog_async=True,
        oplog_nonlogged=True,
    )
    try:
        coll = _client(srv)["t"]["c"]
        with coll.watch() as stream:
            coll.insert_many([{"_id": i, "x": i} for i in range(20)])
            events = [stream.next() for _ in range(20)]
        assert [e["documentKey"]["_id"] for e in events] == list(range(20))
        assert all(e["operationType"] == "insert" for e in events)
        assert coll.count_documents({}) == 20
    finally:
        srv.stop()


def test_checkpoint_seconds_kwarg_accepted(tmp_path, monkeypatch) -> None:
    """data_nonlogged + checkpoint_seconds kwargs open, serve, and survive a
    clean restart (mode recorded in the store, not the environment)."""
    monkeypatch.delenv("SECANTUS_DATA_NONLOGGED", raising=False)
    home = str(tmp_path / "wt")
    srv = _server.RustServer(home, 0, data_nonlogged=True, checkpoint_seconds=3600)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many([{"_id": i} for i in range(10)])
    finally:
        srv.stop()
    # Reopen with no mode kwargs: the recorded mode wins; data intact.
    srv = _server.RustServer(home, 0)
    try:
        assert _client(srv)["t"]["c"].count_documents({}) == 10
    finally:
        srv.stop()
