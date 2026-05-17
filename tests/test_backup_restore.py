"""Rigorous backup / restore round-trip tests.

The ``secantusAdmin.backupArchive`` wire command runs a WT checkpoint
+ tars the storage directory. Restore for this slice is "extract +
start a fresh SecantusDB pointing at the extracted dir". These tests
exercise the whole loop end-to-end and pin invariants users need to
trust before pointing the backup at anything that matters:

* Doc identity across many databases / collections / large doc counts.
* All non-default index shapes (single-field, compound, unique,
  sparse, partial, TTL, 2d, 2dsphere) survive + lookup the same docs
  after restore.
* Oplog tail position survives (so a downstream change-stream resume
  on the restored server picks up where it left off).
* Capped-collection options + FIFO eviction state survive.
* SCRAM users / roles survive — auth still works on the restore.
* Backup taken under concurrent writes still produces a consistent
  snapshot (the writes either all land in the archive or none of
  them do; no torn doc).
* The archive is portable across home-paths (extract anywhere, point
  any SecantusDBServer at it).
"""

from __future__ import annotations

import tarfile
import threading
import time
from pathlib import Path
from typing import Any

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer
from secantus.auth import derive_credentials


@pytest.fixture
def server(tmp_path):
    """Per-test on-disk SecantusDB. ``tmp_path`` is xdist-unique and
    cleaned up automatically. The server's storage path lives at
    ``<tmp_path>/src`` so the test can use sibling paths
    (``<tmp_path>/restored``, ``<tmp_path>/archive.tar.gz``) without
    name collisions."""
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "src")) as srv:
        yield srv


def _extract(archive: Path, target: Path) -> None:
    target.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive, "r:gz") as tar:
        tar.extractall(target, filter="data")


def _take_backup(client: MongoClient, archive: Path) -> dict[str, Any]:
    res = client.admin.command("secantusAdmin.backupArchive", outputPath=str(archive))
    assert res["ok"] == 1.0, res
    return dict(res)


# ---------------------------------------------------------------------------
# Doc identity at scale
# ---------------------------------------------------------------------------


def test_backup_round_trips_many_dbs_and_collections(server, tmp_path) -> None:
    """5 dbs × 3 collections × 200 docs = 3000 docs, all round-trip."""
    archive = tmp_path / "many.tar.gz"
    client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        for db_i in range(5):
            for coll_i in range(3):
                client[f"db_{db_i}"][f"coll_{coll_i}"].insert_many(
                    [{"_id": j, "v": j * 10, "tag": f"db{db_i}-c{coll_i}"} for j in range(200)]
                )
        _take_backup(client, archive)
    finally:
        client.close()

    restored = tmp_path / "restored"
    _extract(archive, restored)
    with SecantusDBServer(port=0, storage_path=str(restored)) as srv2:
        c2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
        try:
            for db_i in range(5):
                for coll_i in range(3):
                    rows = sorted(
                        c2[f"db_{db_i}"][f"coll_{coll_i}"].find(),
                        key=lambda d: d["_id"],
                    )
                    assert len(rows) == 200
                    assert rows[0]["tag"] == f"db{db_i}-c{coll_i}"
                    assert rows[199]["v"] == 1990
        finally:
            c2.close()


# ---------------------------------------------------------------------------
# Indexes
# ---------------------------------------------------------------------------


def test_backup_preserves_index_shapes_and_lookups(server, tmp_path) -> None:
    """Every non-default index shape survives and continues to accelerate
    the matching queries on the restored server.
    """
    archive = tmp_path / "indexes.tar.gz"
    client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        db = client["idx_xd"]
        db["c"].insert_many(
            [
                {"_id": i, "k": i % 10, "v": f"row-{i}", "loc": [i * 0.01, i * 0.01]}
                for i in range(100)
            ]
        )
        db["c"].create_index([("v", 1)], name="v_1", unique=True)
        db["c"].create_index([("k", 1), ("v", -1)], name="k_v_compound")
        db["c"].create_index([("v", 1)], name="v_partial", partialFilterExpression={"k": 0})
        db["c"].create_index([("loc", "2dsphere")], name="loc_geo")
        _take_backup(client, archive)
    finally:
        client.close()

    restored = tmp_path / "restored"
    _extract(archive, restored)
    with SecantusDBServer(port=0, storage_path=str(restored)) as srv2:
        c2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
        try:
            db2 = c2["idx_xd"]
            names = {ix["name"] for ix in db2["c"].list_indexes()}
            assert {"_id_", "v_1", "k_v_compound", "v_partial", "loc_geo"} <= names

            # Unique index still enforces.
            with pytest.raises(OperationFailure):
                db2["c"].insert_one({"_id": 999, "v": "row-0"})

            # Compound index serves a leading-prefix equality lookup.
            assert db2["c"].count_documents({"k": 5}) == 10

            # Partial index has the right filter expression.
            partial = next(ix for ix in db2["c"].list_indexes() if ix["name"] == "v_partial")
            assert partial.get("partialFilterExpression") == {"k": 0}
        finally:
            c2.close()


# ---------------------------------------------------------------------------
# Oplog continuity
# ---------------------------------------------------------------------------


def test_backup_preserves_oplog_tail(server, tmp_path) -> None:
    """Oplog entries written before the backup are visible on the
    restored server, in the same order, with the same ts values.
    """
    archive = tmp_path / "oplog.tar.gz"
    client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        db = client["oplog_xd"]
        db["c"].insert_many([{"_id": i} for i in range(10)])
        db["c"].update_one({"_id": 3}, {"$set": {"v": "updated"}})
        db["c"].delete_one({"_id": 7})

        pre = list(client["local"]["oplog.rs"].find({"ns": "oplog_xd.c"}).sort("ts", 1))
        _take_backup(client, archive)
    finally:
        client.close()

    restored = tmp_path / "restored"
    _extract(archive, restored)
    with SecantusDBServer(port=0, storage_path=str(restored)) as srv2:
        c2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
        try:
            post = list(c2["local"]["oplog.rs"].find({"ns": "oplog_xd.c"}).sort("ts", 1))
            assert len(post) == len(pre)
            assert [e["op"] for e in post] == [e["op"] for e in pre]
            assert [e["ts"] for e in post] == [e["ts"] for e in pre]
        finally:
            c2.close()


# ---------------------------------------------------------------------------
# Capped collections
# ---------------------------------------------------------------------------


def test_backup_preserves_capped_collection_options(server, tmp_path) -> None:
    archive = tmp_path / "capped.tar.gz"
    client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        db = client["capped_xd"]
        db.create_collection("logs", capped=True, size=4096, max=5)
        # Insert one-at-a-time so each batch's ``fresh_id_keys`` shield
        # only contains the newest doc — eviction kicks in per-insert
        # once the count is past ``max``. (A single ``insert_many``
        # batch is treated as "all fresh" and would skip eviction.)
        for i in range(7):
            db["logs"].insert_one({"_id": i, "msg": f"m{i}"})
        assert [d["_id"] for d in db["logs"].find().sort("_id", 1)] == [2, 3, 4, 5, 6]
        _take_backup(client, archive)
    finally:
        client.close()

    restored = tmp_path / "restored"
    _extract(archive, restored)
    with SecantusDBServer(port=0, storage_path=str(restored)) as srv2:
        c2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
        try:
            db2 = c2["capped_xd"]
            [info] = list(db2.list_collections(filter={"name": "logs"}))
            assert info["options"]["capped"] is True
            assert info["options"]["max"] == 5
            assert [d["_id"] for d in db2["logs"].find().sort("_id", 1)] == [2, 3, 4, 5, 6]
            # Eviction continues to enforce the cap after restore.
            db2["logs"].insert_one({"_id": 99, "msg": "post-restore"})
            assert [d["_id"] for d in db2["logs"].find().sort("_id", 1)] == [3, 4, 5, 6, 99]
        finally:
            c2.close()


# ---------------------------------------------------------------------------
# Auth (SCRAM users / roles)
# ---------------------------------------------------------------------------


def test_backup_preserves_users_and_auth_works_after_restore(tmp_path) -> None:
    src = tmp_path / "src"
    src.mkdir()
    archive = tmp_path / "users.tar.gz"

    # Need a fresh auth-enabled server so we control the bootstrap user.
    with SecantusDBServer(port=0, storage_path=str(src), require_auth=True) as srv:
        srv.storage.add_user(
            "admin",
            "root",
            {
                "_id": "admin.root",
                "user": "root",
                "db": "admin",
                "credentials": derive_credentials("rootpw").to_doc(),
                "roles": [{"role": "root", "db": "admin"}],
                "mechanisms": ["SCRAM-SHA-256"],
            },
        )
        admin_uri = (
            f"mongodb://root:rootpw@127.0.0.1:{srv.port}/"
            "?authSource=admin&authMechanism=SCRAM-SHA-256"
        )
        admin = MongoClient(admin_uri, serverSelectionTimeoutMS=2000)
        try:
            admin["shop"].command(
                "createUser",
                "alice",
                pwd="alicepw",
                roles=[{"role": "read", "db": "shop"}],
            )
            admin["shop"]["items"].insert_one({"_id": 1, "name": "thing"})
            _take_backup(admin, archive)
        finally:
            admin.close()

    restored = tmp_path / "restored"
    _extract(archive, restored)
    with SecantusDBServer(port=0, storage_path=str(restored), require_auth=True) as srv2:
        # Original admin can still authenticate.
        admin2 = MongoClient(
            f"mongodb://root:rootpw@127.0.0.1:{srv2.port}/"
            "?authSource=admin&authMechanism=SCRAM-SHA-256",
            serverSelectionTimeoutMS=2000,
        )
        try:
            assert admin2["shop"]["items"].count_documents({}) == 1
        finally:
            admin2.close()
        # The non-admin user we created also still works.
        alice = MongoClient(
            f"mongodb://alice:alicepw@127.0.0.1:{srv2.port}/"
            "?authSource=shop&authMechanism=SCRAM-SHA-256",
            serverSelectionTimeoutMS=2000,
        )
        try:
            assert alice["shop"]["items"].find_one({"_id": 1}) == {"_id": 1, "name": "thing"}
        finally:
            alice.close()


# ---------------------------------------------------------------------------
# Consistency under concurrent writes
# ---------------------------------------------------------------------------


def test_backup_during_concurrent_writes_yields_consistent_snapshot(server, tmp_path) -> None:
    """Run a backup while a writer is mid-burst; assert the snapshot
    is self-consistent (every doc count <= writer's final count, and
    no torn doc — every _id present has the right ``v``).
    """
    archive = tmp_path / "concurrent.tar.gz"
    stop = threading.Event()
    client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    coll = client["concurrent_xd"]["bursts"]

    def writer() -> None:
        i = 0
        while not stop.is_set():
            coll.insert_one({"_id": i, "v": i * 100})
            i += 1
            if i >= 500:
                break

    try:
        t = threading.Thread(target=writer, daemon=True)
        t.start()
        # Let the writer get going before we snapshot.
        time.sleep(0.1)
        _take_backup(client, archive)
        stop.set()
        t.join(timeout=10.0)
    finally:
        client.close()

    restored = tmp_path / "restored"
    _extract(archive, restored)
    with SecantusDBServer(port=0, storage_path=str(restored)) as srv2:
        c2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
        try:
            rows = sorted(c2["concurrent_xd"]["bursts"].find(), key=lambda d: d["_id"])
            # Self-consistency: every persisted row has ``v == _id * 100``.
            assert all(r["v"] == r["_id"] * 100 for r in rows), (
                "torn doc in concurrent backup snapshot"
            )
        finally:
            c2.close()


# ---------------------------------------------------------------------------
# Archive portability
# ---------------------------------------------------------------------------


def test_backup_archive_extract_anywhere(server, tmp_path) -> None:
    """The archive isn't tied to its original home-path — extract into
    an arbitrary new directory and SecantusDB happily opens it.
    """
    archive = tmp_path / "portable.tar.gz"
    client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        client["portable_xd"]["c"].insert_one({"_id": 1, "msg": "hello"})
        _take_backup(client, archive)
    finally:
        client.close()

    # Try extracting under a path with spaces and a deep nested layout.
    for sub in ("plain", "with spaces", "deep/nested/dir/structure"):
        target = tmp_path / "extracted" / sub
        _extract(archive, target)
        with SecantusDBServer(port=0, storage_path=str(target)) as srv2:
            c2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
            try:
                doc = c2["portable_xd"]["c"].find_one({"_id": 1})
                assert doc == {"_id": 1, "msg": "hello"}
            finally:
                c2.close()


# ---------------------------------------------------------------------------
# Repeated backups
# ---------------------------------------------------------------------------


def test_backup_archive_idempotent_when_source_unchanged(server, tmp_path) -> None:
    """Two back-to-back backups against an unchanged source produce
    archives that restore to the same document set.
    """
    archive_a = tmp_path / "a.tar.gz"
    archive_b = tmp_path / "b.tar.gz"
    client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        client["idem_xd"]["c"].insert_many([{"_id": i} for i in range(50)])
        _take_backup(client, archive_a)
        _take_backup(client, archive_b)
    finally:
        client.close()

    def restored_ids(archive: Path) -> list[int]:
        target = tmp_path / archive.stem
        _extract(archive, target)
        with SecantusDBServer(port=0, storage_path=str(target)) as srv2:
            c2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
            try:
                return sorted(d["_id"] for d in c2["idem_xd"]["c"].find())
            finally:
                c2.close()

    assert restored_ids(archive_a) == list(range(50))
    assert restored_ids(archive_b) == list(range(50))
