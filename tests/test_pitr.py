"""Point-in-time recovery (PITR): restore a fresh data dir as the database was
at a target time by replaying the oplog forward.

These tests pin the behaviour users must trust:

* Restoring to a captured timestamp reproduces the exact historical state —
  documents, in-place updates, deletes, and index DDL.
* Restoring with no bound replays the whole oplog ("latest").
* A multi-document transaction is replayed all-or-nothing — the timestamp cut
  never splits it (every statement shares one commit ``ts``).
* Backup archives carry a PITR manifest and restore through it.
* A front-pruned oplog (or a source with no oplog) fails loudly rather than
  silently rebuilding a partial database.
* The wire command and the CLI subcommand both work end-to-end.

Each test uses an xdist-unique ``tmp_path`` and on-disk WiredTiger.
"""

from __future__ import annotations

import json
import tarfile
from pathlib import Path
from typing import Any

import pytest
from bson import Timestamp
from pymongo import MongoClient

from secantus import SecantusDBServer, cli, oplog_replay
from secantus.storage import Storage


def _last_ts(s: Storage) -> Timestamp:
    """Timestamp of the most-recently-emitted oplog entry — a precise mark that
    (unlike ``peek_cluster_time``) is strictly below any later entry's ts."""
    tail = s.oplog_tail_seq()
    rows = s.read_oplog(start_seq=tail, limit=1)
    assert rows, "oplog empty"
    return rows[0][1]["ts"]


def _docs(path: Path, db: str, coll: str) -> list[dict[str, Any]]:
    s = Storage(str(path), enable_oplog=True)
    try:
        return sorted(s.find_matching(db, coll, {}), key=lambda d: d["_id"])
    finally:
        s.close()


def _index_names(path: Path, db: str, coll: str) -> list[str]:
    s = Storage(str(path), enable_oplog=True)
    try:
        return sorted(i["name"] for i in s.list_indexes(db, coll))
    finally:
        s.close()


def test_restore_to_marks_reproduces_history(tmp_path: Path) -> None:
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.insert("app", "c", [{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
    t1 = _last_ts(s)
    s.create_index("app", "c", "v_1", {"v": 1})
    s.update_matching("app", "c", {"_id": 1}, {"$set": {"v": 100, "tag": "x"}})
    t2 = _last_ts(s)
    s.delete_matching("app", "c", {"_id": 2}, limit=1)
    s.insert("app", "c", [{"_id": 3, "v": 3}])
    s.close()

    r1 = tmp_path / "r1"
    oplog_replay.restore_to_timestamp(str(src), str(r1), to_ts=t1)
    assert _docs(r1, "app", "c") == [{"_id": 1, "v": 1}, {"_id": 2, "v": 2}]
    assert _index_names(r1, "app", "c") == ["_id_"]  # index not yet created at t1

    r2 = tmp_path / "r2"
    oplog_replay.restore_to_timestamp(str(src), str(r2), to_ts=t2)
    assert _docs(r2, "app", "c") == [
        {"_id": 1, "v": 100, "tag": "x"},
        {"_id": 2, "v": 2},
    ]
    assert _index_names(r2, "app", "c") == ["_id_", "v_1"]


def test_restore_latest_replays_everything(tmp_path: Path) -> None:
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.insert("app", "c", [{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
    s.update_matching("app", "c", {"_id": 1}, {"$set": {"v": 99}})
    s.delete_matching("app", "c", {"_id": 2}, limit=1)
    s.insert("app", "c", [{"_id": 3, "v": 3}])
    s.close()

    out = tmp_path / "restored"
    stats = oplog_replay.restore_to_timestamp(str(src), str(out))
    assert stats["opsApplied"] == 5  # 2 inserts + update + delete + insert
    assert _docs(out, "app", "c") == [{"_id": 1, "v": 99}, {"_id": 3, "v": 3}]


def test_restore_to_wall_time_bounds(tmp_path: Path) -> None:
    import datetime as dt

    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.insert("app", "c", [{"_id": 1}, {"_id": 2}])
    s.close()

    # Far-future wall bound replays everything; epoch bound replays nothing.
    # (Coarse bounds keep the wall-clock cut deterministic — entry walls have
    # only millisecond resolution, so a precise mid-stream wall cut is fuzzy.)
    future = dt.datetime(2100, 1, 1, tzinfo=dt.timezone.utc)
    past = dt.datetime(1970, 1, 1, tzinfo=dt.timezone.utc)

    r_all = tmp_path / "r_all"
    oplog_replay.restore_to_timestamp(str(src), str(r_all), to_wall=future)
    assert _docs(r_all, "app", "c") == [{"_id": 1}, {"_id": 2}]

    r_none = tmp_path / "r_none"
    oplog_replay.restore_to_timestamp(str(src), str(r_none), to_wall=past)
    assert _docs(r_none, "app", "c") == []


def test_transaction_is_all_or_nothing(tmp_path: Path) -> None:
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.insert("app", "c", [{"_id": 1, "v": 1}])
    before = _last_ts(s)
    handle = s.begin_user_transaction()
    with s.use_user_transaction(handle):
        s.insert("app", "tx", [{"_id": 10}])
        s.insert("app", "tx", [{"_id": 11}])
    s.commit_user_transaction(handle)
    after = _last_ts(s)  # both txn statements share this commit ts
    s.close()

    # Restoring to just-before the transaction excludes it entirely.
    r_before = tmp_path / "r_before"
    oplog_replay.restore_to_timestamp(str(src), str(r_before), to_ts=before)
    assert _docs(r_before, "app", "tx") == []

    # Restoring to the transaction's commit ts includes all its statements.
    r_after = tmp_path / "r_after"
    oplog_replay.restore_to_timestamp(str(src), str(r_after), to_ts=after)
    assert _docs(r_after, "app", "tx") == [{"_id": 10}, {"_id": 11}]


def test_collmod_ttl_and_rename_replayed(tmp_path: Path) -> None:
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.insert("app", "c", [{"_id": 1}])
    s.create_index("app", "c", "t_1", {"t": 1}, {"expireAfterSeconds": 100})
    s.set_index_expiry("app", "c", "t_1", 500)
    s.record_collmod("app", "c", {"index": {"name": "t_1", "expireAfterSeconds": 500}})
    s.rename_collection("app", "c", "app", "renamed")
    s.close()

    out = tmp_path / "restored"
    oplog_replay.restore_to_timestamp(str(src), str(out))
    r = Storage(str(out), enable_oplog=True)
    try:
        assert r.collection_exists("app", "renamed")
        assert not r.collection_exists("app", "c")
        ttl = next(i for i in r.list_indexes("app", "renamed") if i["name"] == "t_1")
        assert ttl["expireAfterSeconds"] == 500
    finally:
        r.close()


def test_pruned_oplog_raises(tmp_path: Path) -> None:
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True, oplog_max_entries=5)
    for i in range(20):
        s.insert("app", "c", [{"_id": i}])
    s.prune_oplog()
    assert s.oplog_floor_seq() > 1
    s.close()

    with pytest.raises(ValueError, match="pruned"):
        oplog_replay.restore_to_timestamp(str(src), str(tmp_path / "restored"))


def test_no_oplog_source_raises(tmp_path: Path) -> None:
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=False)
    s.insert("app", "c", [{"_id": 1}])
    s.close()

    with pytest.raises(ValueError, match="no oplog"):
        oplog_replay.restore_to_timestamp(str(src), str(tmp_path / "restored"))


def test_archive_carries_manifest_and_restores(tmp_path: Path) -> None:
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.insert("app", "c", [{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
    archive = str(tmp_path / "backup.tar.gz")
    s.create_archive(archive)
    s.close()

    with tarfile.open(archive) as tar:
        member = tar.extractfile("pitr-manifest.json")
        assert member is not None
        manifest = json.loads(member.read())
    assert manifest["genesisIntact"] is True
    assert manifest["oplogFloorSeq"] == 1

    out = tmp_path / "restored"
    oplog_replay.restore_archive_to_timestamp(archive, str(out))
    assert _docs(out, "app", "c") == [{"_id": 1, "v": 1}, {"_id": 2, "v": 2}]


def test_cli_restore_from_archive(tmp_path: Path) -> None:
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.insert("app", "c", [{"_id": 1, "v": 1}])
    t1 = _last_ts(s)
    s.insert("app", "c", [{"_id": 2, "v": 2}])
    archive = str(tmp_path / "backup.tar.gz")
    s.create_archive(archive)
    s.close()

    out = tmp_path / "restored"
    rc = cli.main(
        [
            "restore",
            "--source",
            archive,
            "--target-dir",
            str(out),
            "--to-timestamp",
            f"{t1.time},{t1.inc}",
        ]
    )
    assert rc == 0
    assert _docs(out, "app", "c") == [{"_id": 1, "v": 1}]


def test_wire_restore_to_timestamp(tmp_path: Path) -> None:
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "src")) as srv:
        client = MongoClient(srv.uri, directConnection=True)
        coll = client["app"]["c"]
        coll.insert_many([{"_id": 1, "v": 1}, {"_id": 2, "v": 2}])
        coll.update_one({"_id": 1}, {"$set": {"v": 50}})
        archive = str(tmp_path / "backup.tar.gz")
        assert (
            client["admin"].command({"secantusAdmin.backupArchive": 1, "outputPath": archive})["ok"]
            == 1.0
        )
        out = tmp_path / "restored"
        reply = client["admin"].command(
            {"secantusAdmin.restoreToTimestamp": 1, "source": archive, "targetDir": str(out)}
        )
    assert reply["ok"] == 1.0
    assert reply["opsApplied"] == 3
    assert _docs(out, "app", "c") == [{"_id": 1, "v": 50}, {"_id": 2, "v": 2}]


def test_restored_dir_runs_a_server(tmp_path: Path) -> None:
    """A restored data dir is a real, startable database."""
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.insert("shop", "orders", [{"_id": 1, "total": 10}, {"_id": 2, "total": 20}])
    s.close()

    out = tmp_path / "restored"
    oplog_replay.restore_to_timestamp(str(src), str(out))
    with SecantusDBServer(port=0, storage_path=str(out)) as srv:
        client = MongoClient(srv.uri, directConnection=True)
        got = sorted(client["shop"]["orders"].find({}), key=lambda d: d["_id"])
    assert got == [{"_id": 1, "total": 10}, {"_id": 2, "total": 20}]
