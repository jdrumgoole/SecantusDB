"""Cluster-clock restart monotonicity and unbounded write-conflict retry.

Companions to the oplog-meta hotspot removal: ``current_cluster_time`` no
longer persists the meta row per call (it ran on every ``hello`` heartbeat
and change-stream poll — a single-row write hotspot under concurrent
writers), so recovery bumps the clock one second past everything it can
see, and the write-conflict retry loop is unbounded like mongod's
``writeConflictRetry``.
"""

from __future__ import annotations

import pytest

from secantus import storage as storage_mod
from secantus.storage import Storage, WriteConflictError, _retry_write_conflicts


def _meta_blob(store: Storage) -> bytes:
    c = store._cursor(storage_mod._OPLOG_META_TABLE)
    c.set_key("state")
    if c.search() == 0:
        return bytes(c.get_value())
    return b""


def test_cluster_time_mint_does_not_write_meta(tmp_path) -> None:
    store = Storage(str(tmp_path))
    try:
        store.insert("db", "c", [{"_id": 1}])
        store._persist_oplog_meta()
        before = _meta_blob(store)
        for _ in range(5):
            store.current_cluster_time()
        assert _meta_blob(store) == before
    finally:
        store.close()


def test_clock_monotonic_across_reopen_with_unpersisted_mints(tmp_path) -> None:
    """Simulated crash: mints after the last meta persist must never be
    re-minted by the next incarnation. The recovery bump (+1s past
    max(meta, oplog tail, wall clock)) is what guarantees this even when
    the crash and reopen land inside the same wall-clock second."""
    store = Storage(str(tmp_path))
    store.insert("db", "c", [{"_id": 1}])
    # From here on, nothing persists the meta row (crash semantics).
    store._persist_oplog_meta = lambda: None  # type: ignore[method-assign]
    last = None
    for _ in range(50):
        last = store.current_cluster_time()
    store.close()

    reopened = Storage(str(tmp_path))
    try:
        first = reopened.current_cluster_time()
        assert (first.time, first.inc) > (last.time, last.inc)
    finally:
        reopened.close()


def test_write_conflict_retry_is_unbounded() -> None:
    """2000 consecutive conflicts (~40 virtual seconds of backoff — far
    beyond the removed 5s deadline) must retry through to success, never
    surfacing WriteConflict for a plain write."""

    class _FakeTime:
        def __init__(self) -> None:
            self.now = 0.0

        def monotonic(self) -> float:
            return self.now

        def sleep(self, s: float) -> None:
            self.now += s

    class _Dummy:
        class _Tls:
            user_txn = None

        _tls = _Tls()

    calls = {"n": 0}

    @_retry_write_conflicts
    def flaky(self: object) -> str:
        calls["n"] += 1
        if calls["n"] <= 2000:
            raise WriteConflictError("conflict between concurrent operations")
        return "done"

    fake = _FakeTime()
    real = storage_mod._time
    storage_mod._time = fake  # type: ignore[assignment]
    try:
        assert flaky(_Dummy()) == "done"
    finally:
        storage_mod._time = real  # type: ignore[assignment]
    assert calls["n"] == 2001
    assert fake.now > 30.0


def test_write_conflict_inside_user_txn_not_retried() -> None:
    class _Dummy:
        class _Tls:
            user_txn = object()

        _tls = _Tls()

    @_retry_write_conflicts
    def flaky(self: object) -> str:
        raise WriteConflictError("conflict")

    with pytest.raises(WriteConflictError):
        flaky(_Dummy())
