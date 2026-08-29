"""Oplog visibility-point tests for the Python server's storage engine.

The twin of the Rust suite (``crates/secantus-storage/tests/oplog_visibility.rs``,
PR #696): since the Phase-2.4 per-collection lock split, writers on different
collections mint and commit independently, so the sync tail must never pass a
minted-but-uncommitted entry — a reader that advanced over the hole would
permanently lose the event when its transaction commits — and a rolled-back
mint must not stall the tail. Python's batch transactions are held open via
the generator protocol to create the deterministic in-flight window.
Against the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import threading
import time

from bson import Timestamp

from secantus.storage import Storage


def _entry(ns: str, _id: int) -> dict:
    return {"op": "i", "ns": ns, "o": {"_id": _id}, "o2": {"_id": _id}}


def _seqs(storage: Storage, start: int) -> list[int]:
    return [seq for seq, _ in storage.read_oplog(start_seq=start, limit=100)]


def _open_batch_txn_with_emit(storage: Storage, _id: int):
    """Begin a batch transaction on THIS thread and emit one oplog entry
    inside it, leaving the transaction open. Returns the context manager
    (exit it to commit; exit with an exception to roll back)."""
    cm = storage._batch_transaction()
    cm.__enter__()
    storage._emit_oplog([_entry("app.x", _id)])
    return cm


def test_in_flight_batch_txn_pins_visible_tail(tmp_path):
    s = Storage(str(tmp_path / "wt"))
    try:
        # seq 1: plain committed insert (baseline position).
        s.insert("app", "x", [{"_id": 0}])
        assert s.oplog_visible_tail_seq() == 1

        # seq 2: minted inside an OPEN batch transaction — uncommitted.
        cm = _open_batch_txn_with_emit(s, 1)
        try:
            # seq 3: a different collection commits AFTER the in-flight mint
            # (another thread, its own per-collection lock — no shared lock).
            t = threading.Thread(target=lambda: s.insert("app", "y", [{"_id": 2}]))
            t.start()
            t.join()

            # The visible tail must stay pinned at 1: seq 2 is in flight, so
            # reporting 3 would let a reader advance past the hole and lose 2.
            assert s.oplog_visible_tail_seq() == 1, "tail passed a minted-but-uncommitted entry"
            # The scan must not serve seq 3 across the hole, and scan_high
            # (the change-stream skip bound) must not pass it either.
            rows, scan_high = s.read_oplog_scan(start_seq=2, limit=100)
            assert rows == [], f"served rows past an in-flight mint: {rows}"
            assert scan_high <= 1, f"scan_high {scan_high} passed the in-flight mint"
        finally:
            cm.__exit__(None, None, None)  # commit

        # Commit resolves the hole: everything visible, in order.
        assert s.oplog_visible_tail_seq() == 3
        assert _seqs(s, 2) == [2, 3]
    finally:
        s.close()


def test_rolled_back_batch_txn_does_not_stall_tail(tmp_path):
    s = Storage(str(tmp_path / "wt"))
    try:
        s.insert("app", "x", [{"_id": 0}])

        # seq 2 minted in a transaction that rolls back — never visible.
        cm = _open_batch_txn_with_emit(s, 1)
        boom = RuntimeError("abort")
        try:
            raise boom
        except RuntimeError:
            import sys

            cm.__exit__(*sys.exc_info())  # rollback

        # seq 3: committed after the abandoned mint.
        s.insert("app", "y", [{"_id": 2}])

        # The abandoned range must not pin the tail forever.
        assert s.oplog_visible_tail_seq() == 3, "tail stalled on a rolled-back mint"
        assert _seqs(s, 2) == [3]
    finally:
        s.close()


def test_find_seq_for_ts_waits_for_in_flight_mint(tmp_path):
    s = Storage(str(tmp_path / "wt"))
    try:
        # seq 1 (committed): anchors the target ts.
        s.insert("app", "x", [{"_id": 0}])
        ts1 = s.read_oplog(start_seq=1, limit=1)[0][1]["ts"]
        target = Timestamp(ts1.time, ts1.inc + 1)

        # seq 2: minted in an OPEN transaction; its ts satisfies the target,
        # but the committed view's first match is seq 3.
        cm = _open_batch_txn_with_emit(s, 1)
        ty = threading.Thread(target=lambda: s.insert("app", "y", [{"_id": 2}]))
        ty.start()
        ty.join()  # seq 3, committed on its own thread/session

        # find_seq_for_ts runs on another thread (the batch txn must be
        # exited on its owning thread); the commit lands mid-wait.
        # The wait bound is widened well past the 0.12s below so the deadline
        # cannot be what ends the wait. NOTE: widening it did NOT stop this
        # test failing intermittently on Windows CI, which rules the deadline
        # out as the cause — see tasks/backlog.md, "oplog in-flight window
        # races on Windows". Reaching the committed-view answer (seq 3) with a
        # 30s bound means the visible tail did not have seq 2 registered as
        # in flight, which is a real product race, not a slow runner.
        result: list[int] = []
        t = threading.Thread(
            target=lambda: result.append(s.find_seq_for_ts(target, max_wait_seconds=30.0))
        )
        t.start()
        time.sleep(0.12)
        cm.__exit__(None, None, None)  # commit seq 2
        t.join()
        assert result == [2], f"startAtOperationTime finalised past an in-flight mint: {result}"
    finally:
        s.close()


def test_find_seq_for_ts_rolled_back_mint_returns_committed_answer(tmp_path):
    s = Storage(str(tmp_path / "wt"))
    try:
        s.insert("app", "x", [{"_id": 0}])
        ts1 = s.read_oplog(start_seq=1, limit=1)[0][1]["ts"]
        target = Timestamp(ts1.time, ts1.inc + 1)

        cm = _open_batch_txn_with_emit(s, 1)
        ty = threading.Thread(target=lambda: s.insert("app", "y", [{"_id": 2}]))
        ty.start()
        ty.join()  # seq 3, committed on its own thread/session

        result: list[int] = []
        t = threading.Thread(target=lambda: result.append(s.find_seq_for_ts(target)))
        t.start()
        time.sleep(0.12)
        import sys

        try:
            raise RuntimeError("abort")
        except RuntimeError:
            cm.__exit__(*sys.exc_info())  # rollback seq 2
        t.join()
        assert result == [3], f"rolled-back mint should leave the committed answer: {result}"
    finally:
        s.close()


def test_user_txn_commit_is_exactly_once_and_ordered(tmp_path):
    """The buffered user-transaction path: entries mint at commit, AFTER any
    concurrently-committed plain write, and the flush's own in-flight window
    resolves at commit — nothing lost, nothing duplicated, order preserved."""
    s = Storage(str(tmp_path / "wt"))
    try:
        s.insert("app", "x", [{"_id": 0}])  # seq 1

        handle = s.begin_user_transaction()
        with s.use_user_transaction(handle):
            s.insert("app", "x", [{"_id": 1}])  # buffered, no seq yet
        s.insert("app", "y", [{"_id": 2}])  # seq 2, committed first
        s.commit_user_transaction(handle)  # seq 3 minted at commit

        assert s.oplog_visible_tail_seq() == 3
        rows = s.read_oplog(start_seq=1, limit=100)
        ids = [(e["ns"], e["o"]["_id"]) for _, e in rows if e.get("op") == "i"]
        assert ids == [("app.x", 0), ("app.y", 2), ("app.x", 1)]
    finally:
        s.close()


def test_find_seq_for_ts_does_not_accept_a_scan_from_before_the_commit(tmp_path):
    """The visible tail must be sampled BEFORE the scan, not after.

    Sampling it after left a window: an in-flight mint could commit between
    the two reads, so the scan still returned the pre-commit answer (the seq
    *above* the in-flight one) while the tail read afterwards had already
    advanced to cover it. The stale answer then passed the check and the
    entry was skipped for good — silent change-stream event loss.

    The interleaving is forced rather than raced for. It was originally found
    as an intermittent Windows CI failure, where the wider scheduling quantum
    hit the window by chance; with the reads in the wrong order this test
    fails every time on every platform.
    """
    s = Storage(str(tmp_path / "wt"))
    try:
        s.insert("app", "x", [{"_id": 0}])
        ts1 = s.read_oplog(start_seq=1, limit=1)[0][1]["ts"]
        target = Timestamp(ts1.time, ts1.inc + 1)

        # seq 2 minted in an open transaction; seq 3 committed above it.
        cm = _open_batch_txn_with_emit(s, 1)
        ty = threading.Thread(target=lambda: s.insert("app", "y", [{"_id": 2}]))
        ty.start()
        ty.join()

        scanned = threading.Event()
        commit_done = threading.Event()
        original_scan = s._find_seq_for_ts_scan
        first = [True]

        def scan_then_let_the_commit_land(ts):
            # The scan runs against the PRE-commit view (so it answers 3,
            # the seq above the in-flight mint), and only then does seq 2
            # commit. Whatever reads the visible tail afterwards sees a tail
            # that already covers 3 — so the old order accepted the stale 3.
            # Only the first call is delayed; later loop iterations must run
            # normally or the retry could never observe the commit.
            result_seq = original_scan(ts)
            if first[0]:
                first[0] = False
                scanned.set()
                commit_done.wait(10)
            return result_seq

        s._find_seq_for_ts_scan = scan_then_let_the_commit_land

        result: list[int] = []
        t = threading.Thread(
            target=lambda: result.append(s.find_seq_for_ts(target, max_wait_seconds=30.0))
        )
        t.start()
        assert scanned.wait(10), "scan never ran"
        cm.__exit__(None, None, None)  # commit seq 2 on its owning thread
        commit_done.set()
        t.join(30)

        assert result == [2], f"startAtOperationTime skipped the committed mint: {result}"
    finally:
        s.close()
