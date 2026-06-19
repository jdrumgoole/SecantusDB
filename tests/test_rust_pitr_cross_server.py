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
