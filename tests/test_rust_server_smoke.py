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
