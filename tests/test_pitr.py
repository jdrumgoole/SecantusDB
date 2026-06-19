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
import time
from pathlib import Path
from typing import Any

import pytest
from bson import Timestamp
from pymongo import MongoClient

from secantus import SecantusDBServer, cli, oplog_replay, pitr_archive
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


def test_v2_restore_past_pruned_floor(tmp_path: Path) -> None:
    """PITR v2: with a base snapshot + archived oplog segments, a restore can
    reach a time *before* the live oplog floor — the window v1 refuses."""
    archive = tmp_path / "archive"
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True, oplog_archive_dir=str(archive), oplog_max_entries=2)
    s.insert("app", "c", [{"_id": 1}])
    s.insert("app", "c", [{"_id": 2}])
    s.archive_base_snapshot(str(archive))  # base head = 2
    s.insert("app", "c", [{"_id": 3}])
    t3 = _last_ts(s)  # recovery target: just after doc 3
    s.insert("app", "c", [{"_id": 4}])
    s.insert("app", "c", [{"_id": 5}])
    pruned = s.prune_oplog()  # cap=2 keeps seq 4,5; seq 1,2,3 archived + dropped
    assert pruned >= 3
    assert s.oplog_floor_seq() == 4  # live oplog no longer reaches seq 3
    s.close()

    # v1 can't do this — the source oplog floor is past genesis.
    with pytest.raises(ValueError, match="past genesis"):
        oplog_replay.restore_to_timestamp(str(src), str(tmp_path / "v1"), to_ts=t3)

    # v2 stitches base (docs 1,2) + archived seq 3 → docs 1,2,3 at t3.
    out = tmp_path / "restored"
    stats = pitr_archive.restore_from_archive_dir(str(archive), str(out), to_ts=t3)
    assert stats["baseHeadSeq"] == 2
    assert _docs(out, "app", "c") == [{"_id": 1}, {"_id": 2}, {"_id": 3}]


def test_cli_restore_from_archive_dir(tmp_path: Path) -> None:
    """`secantusdb restore --source <archive-dir>` routes to the v2 path."""
    archive = tmp_path / "archive"
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True, oplog_archive_dir=str(archive), oplog_max_entries=2)
    s.insert("app", "c", [{"_id": 1}])
    s.insert("app", "c", [{"_id": 2}])
    s.insert("app", "c", [{"_id": 3}])
    s.archive_base_snapshot(str(archive))  # newest base has all three
    s.prune_oplog()
    s.close()

    out = tmp_path / "restored"
    rc = cli.main(["restore", "--source", str(archive), "--target-dir", str(out)])
    assert rc == 0
    assert _docs(out, "app", "c") == [{"_id": 1}, {"_id": 2}, {"_id": 3}]


def test_wire_v2_archive_base_snapshot_and_restore(tmp_path: Path) -> None:
    """End-to-end v2 over the wire: a server with --oplog-archive-dir archives
    pruned oplog, `secantusAdmin.archiveBaseSnapshot` takes base snapshots, and
    `secantusAdmin.restoreToTimestamp` rebuilds from the archive directory."""
    archive = tmp_path / "archive"
    src = tmp_path / "src"
    with SecantusDBServer(
        port=0,
        storage_path=str(src),
        oplog_archive_dir=str(archive),
        oplog_max_entries=2,
    ) as srv:
        admin = MongoClient(srv.uri, directConnection=True)["admin"]
        coll = MongoClient(srv.uri, directConnection=True)["app"]["c"]
        coll.insert_one({"_id": 1})
        coll.insert_one({"_id": 2})
        snap = admin.command({"secantusAdmin.archiveBaseSnapshot": 1, "archiveDir": str(archive)})
        assert snap["ok"] == 1.0 and "path" in snap
        coll.insert_one({"_id": 3})
        coll.insert_one({"_id": 4})
        admin.command({"secantusAdmin.archiveBaseSnapshot": 1, "archiveDir": str(archive)})
        admin.command({"secantusAdmin.pruneOplog": 1})  # archives the pruned front
        out = tmp_path / "restored"
        reply = admin.command(
            {"secantusAdmin.restoreToTimestamp": 1, "source": str(archive), "targetDir": str(out)}
        )
    assert reply["ok"] == 1.0
    with SecantusDBServer(port=0, storage_path=str(out)) as srv:
        got = sorted(
            MongoClient(srv.uri, directConnection=True)["app"]["c"].find({}),
            key=lambda d: d["_id"],
        )
    assert got == [{"_id": 1}, {"_id": 2}, {"_id": 3}, {"_id": 4}]


def test_v2_segment_roundtrip(tmp_path: Path) -> None:
    """Archived oplog segments round-trip their rows verbatim in seq order."""
    archive = str(tmp_path / "arch")
    rows = [
        (1, {"op": "i", "ns": "a.b", "o": {"_id": 1}}, None),
        (2, {"op": "i", "ns": "a.b", "o": {"_id": 2}}, {"_id": 2, "old": True}),
    ]
    pitr_archive.write_segment(archive, rows)
    got = list(pitr_archive.iter_archived_oplog(archive))
    assert [(seq, e["o"]["_id"], p) for seq, e, p in got] == [
        (1, 1, None),
        (2, 2, {"_id": 2, "old": True}),
    ]


def _next_event(cs: Any, tries: int = 60) -> dict[str, Any]:
    """Poll a change stream for the next event (the restored oplog already
    holds it, so this returns within a few polls)."""
    for _ in range(tries):
        ev = cs.try_next()
        if ev is not None:
            return ev
        time.sleep(0.05)
    raise AssertionError("no change event arrived")


def test_carry_oplog_preserves_timeline(tmp_path: Path) -> None:
    """``carry_oplog`` writes the replayed oplog verbatim onto the restored
    store — same seqs — and advances the seq counter so a fresh write mints a
    strictly-greater seq. The default (no carry) leaves an empty timeline."""
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.insert("app", "c", [{"_id": 1}])
    s.insert("app", "c", [{"_id": 2}])
    s.insert("app", "c", [{"_id": 3}])
    src_seqs = [seq for seq, _ in s.read_oplog(start_seq=1, limit=100)]
    s.close()

    carried = tmp_path / "carried"
    stats = oplog_replay.restore_to_timestamp(str(src), str(carried), carry_oplog=True)
    assert stats["oplogCarried"] == len(src_seqs)
    r = Storage(str(carried), enable_oplog=True)
    try:
        assert [seq for seq, _ in r.read_oplog(start_seq=1, limit=100)] == src_seqs
        tail = r.oplog_tail_seq()
        r.insert("app", "c", [{"_id": 4}])
        assert r.oplog_tail_seq() == tail + 1  # new write continues the timeline
    finally:
        r.close()

    # Default restore leaves the restored oplog empty (fresh timeline).
    fresh = tmp_path / "fresh"
    stats = oplog_replay.restore_to_timestamp(str(src), str(fresh))
    assert stats["oplogCarried"] == 0
    r = Storage(str(fresh), enable_oplog=True)
    try:
        assert r.read_oplog(start_seq=1, limit=100) == []
    finally:
        r.close()


def test_preserve_oplog_change_stream_resumes_across_restore(tmp_path: Path) -> None:
    """End-to-end: with ``preserveOplog`` a change stream on the restored server
    resumes from a token minted *before* the restore point — the carried oplog
    still holds the rows that token references."""
    src = tmp_path / "src"
    with SecantusDBServer(port=0, storage_path=str(src)) as srv:
        coll = MongoClient(srv.uri, directConnection=True)["app"]["c"]
        coll.insert_one({"_id": 1})
        cs = coll.watch()
        coll.insert_one({"_id": 2})
        token = _next_event(cs)["_id"]  # resume token after the {_id: 2} insert
        cs.close()
        coll.insert_one({"_id": 3})  # happens after the token, before backup
        archive = str(tmp_path / "backup.tar.gz")
        MongoClient(srv.uri, directConnection=True)["admin"].command(
            {"secantusAdmin.backupArchive": 1, "outputPath": archive}
        )

    out = tmp_path / "restored"
    stats = oplog_replay.restore_archive_to_timestamp(archive, str(out), carry_oplog=True)
    assert stats["oplogCarried"] > 0

    with SecantusDBServer(port=0, storage_path=str(out)) as srv:
        coll = MongoClient(srv.uri, directConnection=True)["app"]["c"]
        cs = coll.watch(resume_after=token)
        ev = _next_event(cs)
        cs.close()
        assert ev["operationType"] == "insert"
        assert ev["fullDocument"] == {"_id": 3}


def test_collection_options_replayed(tmp_path: Path) -> None:
    """capped / size / max + a validator set at *create* time survive PITR
    replay. These ride the ``create`` oplog entry — the options blob itself
    is oplog-silent, so before this they were lost on restore."""
    src = tmp_path / "src"
    s = Storage(str(src), enable_oplog=True)
    s.create_collection(
        "app",
        "events",
        options={
            "capped": True,
            "size": 8192,
            "max": 100,
            "validator": {"v": {"$gt": 0}},
        },
    )
    s.insert("app", "events", [{"_id": 1, "v": 5}])
    s.close()

    out = tmp_path / "restored"
    oplog_replay.restore_to_timestamp(str(src), str(out))
    r = Storage(str(out), enable_oplog=True)
    try:
        opts = r.get_collection_options("app", "events")
        assert opts.get("capped") is True
        assert opts.get("size") == 8192
        assert opts.get("max") == 100
        assert opts.get("validator") == {"v": {"$gt": 0}}
        assert r.find_matching("app", "events", {}) == [{"_id": 1, "v": 5}]
    finally:
        r.close()


def test_wire_create_options_survive_restore(tmp_path: Path) -> None:
    """End-to-end: a capped collection with a validator created through the
    driver is reconstructed (options and all) on the restored server, proving
    the wire ``create`` -> oplog -> replay path carries collection options."""
    src = tmp_path / "src"
    with SecantusDBServer(port=0, storage_path=str(src)) as srv:
        db = MongoClient(srv.uri, directConnection=True)["app"]
        db.create_collection(
            "events",
            capped=True,
            size=8192,
            max=100,
            validator={"v": {"$gt": 0}},
        )
        db["events"].insert_one({"_id": 1, "v": 5})

    out = tmp_path / "restored"
    oplog_replay.restore_to_timestamp(str(src), str(out))
    with SecantusDBServer(port=0, storage_path=str(out)) as srv:
        db = MongoClient(srv.uri, directConnection=True)["app"]
        info = next(c for c in db.list_collections() if c["name"] == "events")
        opts = info["options"]
        assert opts.get("capped") is True
        assert opts.get("size") == 8192
        assert opts.get("max") == 100
        assert opts.get("validator") == {"v": {"$gt": 0}}
