"""Cross-server point-in-time recovery (Phase R, R6a).

The Python server and the Rust server share the **exact** WiredTiger schema and
oplog shape, so the Python ``oplog_replay`` restore tooling can rebuild a
database from a *stopped* Rust server's data directory — no Rust-side restore
code required. This test pins that format identity: a database written through
the Rust server is reconstructed, op-for-op, by the Python PITR applier.

Gated on the WiredTiger-linking ``_secantus_server`` extension (built with
``SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON``); skipped in the
WT-less dev sandbox.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

from secantus import oplog_replay  # noqa: E402
from secantus.storage import Storage  # noqa: E402


def _docs(path: Path, db: str, coll: str) -> list[dict[str, Any]]:
    s = Storage(str(path), enable_oplog=True)
    try:
        return sorted(s.find_matching(db, coll, {}), key=lambda d: d["_id"])
    finally:
        s.close()


def _rust_client(srv: Any):
    host, port = srv.address
    return pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)


def test_python_restores_stopped_rust_data_dir(tmp_path: Path) -> None:
    """Write CRUD history through the Rust server, stop it, and rebuild the
    database with the Python restore tool from the stopped data directory."""
    data = tmp_path / "rustdata"
    srv = _server.RustServer(str(data), 0)
    try:
        coll = _rust_client(srv)["app"]["c"]
        coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
        coll.update_one({"_id": 1}, {"$set": {"v": 100, "tag": "x"}})
        coll.delete_one({"_id": 2})
        coll.insert_one({"_id": 3, "v": 3})
    finally:
        srv.stop()  # releases WiredTiger's single-writer lock

    out = tmp_path / "restored"
    oplog_replay.restore_to_timestamp(str(data), str(out))
    assert _docs(out, "app", "c") == [
        {"_id": 1, "v": 100, "tag": "x"},
        {"_id": 3, "v": 3},
    ]


def test_python_restores_rust_data_to_a_mark(tmp_path: Path) -> None:
    """A bounded restore of Rust-written history reproduces the exact state at
    the target oplog timestamp."""
    data = tmp_path / "rustdata"
    srv = _server.RustServer(str(data), 0)
    try:
        coll = _rust_client(srv)["app"]["c"]
        coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
    finally:
        srv.stop()

    # Capture the timestamp after the two inserts from the stopped data dir.
    src = Storage(str(data), enable_oplog=True)
    try:
        tail = src.oplog_tail_seq()
        t_mark = src.read_oplog(start_seq=tail, limit=1)[0][1]["ts"]
    finally:
        src.close()

    out = tmp_path / "restored"
    oplog_replay.restore_to_timestamp(str(data), str(out), to_ts=t_mark)
    assert _docs(out, "app", "c") == [{"_id": 1, "v": 1}, {"_id": 2, "v": 2}]


def test_python_restores_rust_backup_archive(tmp_path: Path) -> None:
    """The Rust server's native ``secantusAdmin.backupArchive`` produces a backup
    archive (taken live, off the WiredTiger ``backup:`` cursor) that the Python
    restore tool rebuilds — proving the Rust-written archive format is identical
    to the Python one (Phase R, R1)."""
    data = tmp_path / "rustdata"
    archive = tmp_path / "rust-backup.tar.gz"
    srv = _server.RustServer(str(data), 0)
    try:
        client = _rust_client(srv)
        coll = client["app"]["c"]
        coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
        coll.update_one({"_id": 1}, {"$set": {"v": 100}})
        # Backup is taken against the live server (a consistent WT snapshot).
        reply = client["admin"].command(
            {"secantusAdmin.backupArchive": 1, "outputPath": str(archive)}
        )
        assert reply["ok"] == 1.0
        assert reply["sizeBytes"] > 0
        assert reply["path"] == str(archive)
    finally:
        srv.stop()

    assert archive.exists()
    out = tmp_path / "restored"
    oplog_replay.restore_archive_to_timestamp(str(archive), str(out))
    assert _docs(out, "app", "c") == [{"_id": 1, "v": 100}, {"_id": 2, "v": 2}]


def test_rust_backup_archive_requires_output_path(tmp_path: Path) -> None:
    """``backupArchive`` without ``outputPath`` is a clean TypeMismatch error."""
    srv = _server.RustServer(str(tmp_path / "data"), 0)
    try:
        reply = _rust_client(srv)["admin"].command({"secantusAdmin.backupArchive": 1}, check=False)
        assert reply["ok"] == 0.0
        assert reply["code"] == 14
    finally:
        srv.stop()


def test_rust_create_options_survive_restore(tmp_path: Path) -> None:
    """A capped collection with a validator, created through the Rust server,
    is reconstructed (options and all) after a backup+restore — proving the Rust
    server carries collection options in the `create` oplog entry (Phase R, R5a)."""
    data = tmp_path / "rustdata"
    archive = tmp_path / "backup.tar.gz"
    srv = _server.RustServer(str(data), 0)
    try:
        db = _rust_client(srv)["app"]
        db.create_collection("events", capped=True, size=8192, max=100, validator={"v": {"$gt": 0}})
        db["events"].insert_one({"_id": 1, "v": 5})
        reply = _rust_client(srv)["admin"].command(
            {"secantusAdmin.backupArchive": 1, "outputPath": str(archive)}
        )
        assert reply["ok"] == 1.0
    finally:
        srv.stop()

    out = tmp_path / "restored"
    oplog_replay.restore_archive_to_timestamp(str(archive), str(out))
    s = Storage(str(out), enable_oplog=True)
    try:
        opts = s.get_collection_options("app", "events")
    finally:
        s.close()
    assert opts.get("capped") is True
    assert opts.get("size") == 8192
    assert opts.get("max") == 100
    assert opts.get("validator") == {"v": {"$gt": 0}}
