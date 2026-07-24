from __future__ import annotations

import tempfile
from pathlib import Path

import pytest
from bson.timestamp import Timestamp

from secantus.storage import Storage


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def test_emit_assigns_monotonic_seq(storage: Storage) -> None:
    seq1 = storage._emit_oplog([{"op": "n", "ns": "a.b"}])
    seq2 = storage._emit_oplog([{"op": "n", "ns": "a.b"}])
    seq3 = storage._emit_oplog([{"op": "n", "ns": "a.b"}, {"op": "n", "ns": "a.b"}])
    assert seq1 == 1
    assert seq2 == 2
    assert seq3 == 4  # last seq emitted in batch
    assert storage.oplog_tail_seq() == 4


def test_emit_assigns_monotonic_ts(storage: Storage) -> None:
    fake_now = [1_000_000.0]
    storage._time = lambda: fake_now[0]
    storage._emit_oplog([{"op": "n", "ns": "a.b"}])
    storage._emit_oplog([{"op": "n", "ns": "a.b"}])
    fake_now[0] = 1_000_001.0  # next second
    storage._emit_oplog([{"op": "n", "ns": "a.b"}])
    rows = storage.read_oplog(start_seq=1, limit=10)
    ts_values = [(e["ts"].time, e["ts"].inc) for _, e in rows]
    assert ts_values == [(1_000_000, 1), (1_000_000, 2), (1_000_001, 1)]


def test_read_oplog_filters_by_ns(storage: Storage) -> None:
    storage._emit_oplog([{"op": "i", "ns": "x.y", "o": {"_id": 1}, "o2": {"_id": 1}}])
    storage._emit_oplog([{"op": "i", "ns": "x.z", "o": {"_id": 2}, "o2": {"_id": 2}}])
    storage._emit_oplog([{"op": "i", "ns": "x.y", "o": {"_id": 3}, "o2": {"_id": 3}}])
    matched = storage.read_oplog(start_seq=1, limit=10, ns_filter=lambda ns: ns == "x.y")
    assert [seq for seq, _ in matched] == [1, 3]


def test_find_seq_for_ts_returns_first_at_or_after(storage: Storage) -> None:
    fake_now = [1_000_000.0]
    storage._time = lambda: fake_now[0]
    storage._emit_oplog([{"op": "n", "ns": "a.b"}])  # seq 1, ts (1000000, 1)
    fake_now[0] = 1_000_005.0
    storage._emit_oplog([{"op": "n", "ns": "a.b"}])  # seq 2, ts (1000005, 1)
    fake_now[0] = 1_000_010.0
    storage._emit_oplog([{"op": "n", "ns": "a.b"}])  # seq 3, ts (1000010, 1)
    assert storage.find_seq_for_ts(Timestamp(1_000_004, 0)) == 2
    assert storage.find_seq_for_ts(Timestamp(1_000_005, 1)) == 2
    assert storage.find_seq_for_ts(Timestamp(1_000_006, 0)) == 3
    assert storage.find_seq_for_ts(Timestamp(2_000_000, 0)) == storage.oplog_tail_seq() + 1


def test_prune_oplog_respects_time_cap(tmp_path) -> None:
    s = Storage(str(tmp_path), oplog_retention_seconds=10.0)
    fake_now = [1_000_000.0]
    s._time = lambda: fake_now[0]
    s._emit_oplog([{"op": "n", "ns": "a.b"}])  # ts secs = 1_000_000
    fake_now[0] = 1_000_005.0
    s._emit_oplog([{"op": "n", "ns": "a.b"}])  # ts secs = 1_000_005
    fake_now[0] = 1_000_020.0
    s._emit_oplog([{"op": "n", "ns": "a.b"}])  # ts secs = 1_000_020
    pruned = s.prune_oplog(now=1_000_020.0)  # cutoff = 1_000_010
    # First two are below cutoff; third is at cutoff.
    assert pruned == 2
    surviving = s.read_oplog(start_seq=1, limit=10)
    assert [seq for seq, _ in surviving] == [3]
    s.close()


def test_prune_oplog_respects_count_cap(tmp_path) -> None:
    s = Storage(str(tmp_path), oplog_max_entries=2, oplog_retention_seconds=10_000.0)
    for _ in range(5):
        s._emit_oplog([{"op": "n", "ns": "a.b"}])
    s.prune_oplog()
    surviving = s.read_oplog(start_seq=1, limit=10)
    assert len(surviving) == 2
    # Newest two retained.
    assert {seq for seq, _ in surviving} == {4, 5}
    s.close()


def test_live_count_tracks_emits_and_prunes(tmp_path) -> None:
    s = Storage(str(tmp_path), oplog_max_entries=3, oplog_retention_seconds=10_000.0)
    for _ in range(10):
        s._emit_oplog([{"op": "n", "ns": "a.b"}])
    assert s._oplog_live_count == 10
    s.prune_oplog()
    # Cap trims to the newest 3, and the in-memory count matches reality.
    assert s._oplog_live_count == 3
    assert len(s.read_oplog(start_seq=1, limit=100)) == 3
    s.close()


def test_live_count_is_reseeded_on_reopen(tmp_path) -> None:
    path = str(tmp_path / "wt-home")
    s1 = Storage(path)
    for _ in range(7):
        s1._emit_oplog([{"op": "n", "ns": "a.b"}])
    s1.close()
    # A fresh instance must recover the live count from the persisted rows,
    # not start at zero — otherwise the first prune's cap decision is wrong.
    s2 = Storage(path, oplog_max_entries=2, oplog_retention_seconds=10_000.0)
    try:
        assert s2._oplog_live_count == 7
        s2.prune_oplog()
        assert s2._oplog_live_count == 2
        assert len(s2.read_oplog(start_seq=1, limit=100)) == 2
    finally:
        s2.close()


def test_prune_reads_only_the_doomed_prefix(tmp_path) -> None:
    # The whole point of the rewrite: a prune that deletes D rows must read
    # ~D+1 entries, never the whole oplog. Spy on the oldest-first generator.
    s = Storage(str(tmp_path), oplog_max_entries=500, oplog_retention_seconds=10_000.0)
    for _ in range(600):
        s._emit_oplog([{"op": "n", "ns": "a.b"}])
    yielded = [0]
    real_iter = s._iter_oplog_oldest

    def counting_iter(session):
        for item in real_iter(session):
            yielded[0] += 1
            yield item

    s._iter_oplog_oldest = counting_iter
    pruned = s.prune_oplog()  # cap surplus = 600 - 500 = 100
    assert pruned == 100
    # Reads the 100 doomed + the one entry that ends the walk — nowhere near 600.
    assert yielded[0] <= 102
    s.close()


def test_prune_oplog_drops_paired_preimages(tmp_path) -> None:
    import bson

    s = Storage(str(tmp_path), oplog_retention_seconds=0.0)  # immediate eligibility
    pre_doc = {"_id": 1, "x": 5}
    s._emit_oplog(
        [{"op": "d", "ns": "a.b", "o": {"_id": 1}, "o2": {"_id": 1}}],
        pre_images=[bson.encode(pre_doc)],
    )
    seq = s.oplog_tail_seq()
    pre = s.read_preimage(seq)
    assert pre == pre_doc
    # Use a far-future "now" so the entry is definitively past cutoff.
    s.prune_oplog(now=10_000_000_000.0)
    assert s.read_preimage(seq) is None
    s.close()


def test_oplog_survives_close_and_reopen() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        path = str(Path(tmp) / "wt-home")
        s1 = Storage(path)
        s1._emit_oplog([{"op": "i", "ns": "a.b", "o": {"_id": 1}, "o2": {"_id": 1}}])
        s1._emit_oplog([{"op": "i", "ns": "a.b", "o": {"_id": 2}, "o2": {"_id": 2}}])
        seqs_before = [seq for seq, _ in s1.read_oplog(start_seq=1, limit=10)]
        last_ts_before = (s1._last_ts_secs, s1._last_ts_ord)
        s1.close()

        s2 = Storage(path)
        seqs_after = [seq for seq, _ in s2.read_oplog(start_seq=1, limit=10)]
        assert seqs_after == seqs_before == [1, 2]
        # Recovery bumps the cluster clock strictly PAST the persisted
        # state (+1s past max(meta, oplog tail, wall clock)) so that
        # cluster-time mints issued after the last persist — which no
        # longer happens per call — can never be re-minted by the next
        # incarnation. Exact restoration was the old contract; strict
        # monotonicity is the new one (mongod's cluster time also jumps
        # on restart).
        assert (s2._last_ts_secs, s2._last_ts_ord) > last_ts_before
        # Continuing must mint a strictly greater seq AND ts.
        s2._emit_oplog([{"op": "i", "ns": "a.b", "o": {"_id": 3}, "o2": {"_id": 3}}])
        assert s2.oplog_tail_seq() == 3
        s2.close()


def test_collection_uuid_persists_within_session(storage: Storage) -> None:
    u1 = storage.collection_uuid("db", "coll")
    u2 = storage.collection_uuid("db", "coll")
    assert u1 == u2
    other = storage.collection_uuid("db", "other")
    assert u1 != other


def test_storage_writes_emit_oplog_with_collection_uuid(storage: Storage) -> None:
    storage.create_collection("db", "coll")
    storage.insert("db", "coll", [{"_id": 1, "x": 10}])
    storage.update_matching("db", "coll", {"_id": 1}, {"$set": {"x": 99}})
    storage.delete_matching("db", "coll", {"_id": 1})
    rows = storage.read_oplog(start_seq=1, limit=20)
    ops = [e["op"] for _, e in rows]
    # create + insert + update + delete (in that order)
    assert ops == ["c", "i", "u", "d"]
    # All non-create entries carry a 16-byte collection UUID
    for _, e in rows[1:]:
        ui = e.get("ui")
        assert ui is not None
        assert len(bytes(ui)) == 16


def test_update_oplog_carries_faithful_diff(storage: Storage) -> None:
    storage.insert("db", "coll", [{"_id": 1, "a": 1, "b": 2}])
    storage.update_matching("db", "coll", {"_id": 1}, {"$set": {"a": 99}, "$unset": {"b": ""}})
    rows = storage.read_oplog(start_seq=1, limit=10)
    update_entries = [e for _, e in rows if e["op"] == "u"]
    assert len(update_entries) == 1
    o = update_entries[0]["o"]
    assert o.get("$v") == 2
    diff = o.get("diff")
    assert diff["updatedFields"] == {"a": 99}
    assert diff["removedFields"] == ["b"]


def test_drop_collection_emits_drop_command(storage: Storage) -> None:
    storage.create_collection("db", "coll")
    storage.insert("db", "coll", [{"_id": 1}])
    storage.drop_collection("db", "coll")
    rows = storage.read_oplog(start_seq=1, limit=20)
    ops = [(e["op"], e.get("o")) for _, e in rows]
    assert ops[-1] == ("c", {"drop": "coll"})


def test_preimage_only_stored_when_enabled(storage: Storage) -> None:
    storage.create_collection("db", "coll")
    storage.insert("db", "coll", [{"_id": 1, "x": 1}])
    storage.update_matching("db", "coll", {"_id": 1}, {"$set": {"x": 2}})
    rows_before = storage.read_oplog(start_seq=1, limit=20)
    update_entries_before = [(seq, e) for seq, e in rows_before if e["op"] == "u"]
    assert len(update_entries_before) == 1
    seq_before, _ = update_entries_before[0]
    assert storage.read_preimage(seq_before) is None

    storage.set_collection_options("db", "coll", changeStreamPreAndPostImages={"enabled": True})
    storage.update_matching("db", "coll", {"_id": 1}, {"$set": {"x": 3}})
    rows_after = storage.read_oplog(start_seq=1, limit=20)
    update_entries_after = [(seq, e) for seq, e in rows_after if e["op"] == "u"]
    assert len(update_entries_after) == 2
    seq_after = update_entries_after[-1][0]
    pre = storage.read_preimage(seq_after)
    assert pre is not None
    assert pre["_id"] == 1
    assert pre["x"] == 2  # the value before the second update
