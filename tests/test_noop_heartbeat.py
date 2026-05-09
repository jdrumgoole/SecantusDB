"""Tests for periodic noop heartbeats on the oplog.

Real ``mongod`` writes ``{op: "n"}`` entries to the oplog every
~10 seconds (configurable via ``periodicNoopIntervalSecs``) so cluster
time advances even on quiet collections. Change-stream consumers skip
the noop in projection but still advance their resume token past it,
so a long-quiet stream's resume token stays inside the oplog retention
window and its ``postBatchResumeToken`` reflects current cluster time.

These tests exercise:

  1. ``Storage.emit_noop_heartbeat()`` writes one ``op: "n"`` entry.
  2. The background thread fires when ``noop_heartbeat_seconds > 0``.
  3. ``changestreams.project`` returns ``None`` for noop rows.
  4. End-to-end: a pymongo change-stream's resume token advances past
     a heartbeat-emitted oplog entry on a collection with no user
     ops.
"""

from __future__ import annotations

import time

import pytest
from bson import Timestamp
from pymongo import MongoClient

from secantus import SecantusDBServer
from secantus.changestreams import project
from secantus.storage import Storage


def test_emit_noop_heartbeat_writes_oplog_entry(tmp_path) -> None:
    storage = Storage(str(tmp_path / "wt"))
    try:
        seq = storage.emit_noop_heartbeat()
        assert seq > 0
        rows = list(storage.read_oplog(start_seq=seq, limit=5))
        assert len(rows) >= 1
        # Find the heartbeat we just emitted.
        match = next((entry for s, entry in rows if s == seq), None)
        assert match is not None
        assert match["op"] == "n"
        assert match["ns"] == ""
        assert match["o"]["msg"] == "periodic noop"
        assert isinstance(match["ts"], Timestamp)
    finally:
        storage.close()


def test_background_heartbeat_thread_fires(tmp_path) -> None:
    """A small heartbeat interval should produce multiple noop oplog
    entries within a short observation window. Generous bounds because
    CI runners on slow Python builds (macos 3.10 in particular) can
    take >100ms to schedule the daemon thread for the first tick;
    the goal here is to prove it fires *repeatedly*, not to time it."""
    storage = Storage(str(tmp_path / "wt"), noop_heartbeat_seconds=0.1)
    try:
        time.sleep(1.5)  # ~12-15 heartbeats expected at 100ms cadence
        rows = list(storage.read_oplog(start_seq=1, limit=100))
        noop_rows = [entry for _seq, entry in rows if entry.get("op") == "n"]
        # Repeated firing requires at least 3 entries (startup + at
        # least two scheduled iterations); the slow-CI safety margin
        # is the 1.5s window vs the 100ms cadence.
        assert len(noop_rows) >= 3, f"expected >=3 heartbeat rows in 1.5s, got {len(noop_rows)}"
    finally:
        storage.close()


def test_heartbeat_thread_does_not_start_when_oplog_disabled(tmp_path) -> None:
    """``enable_oplog=False`` short-circuits the heartbeat thread —
    it would write nothing useful and would just steal CPU."""
    storage = Storage(
        str(tmp_path / "wt"),
        enable_oplog=False,
        noop_heartbeat_seconds=0.05,
    )
    try:
        assert storage._noop_thread is None
    finally:
        storage.close()


def test_heartbeat_thread_disabled_by_default(tmp_path) -> None:
    storage = Storage(str(tmp_path / "wt"))
    try:
        assert storage._noop_thread is None
    finally:
        storage.close()


def test_project_returns_none_for_noop(tmp_path) -> None:
    """Noop oplog rows must not surface as change-stream events;
    projection returns ``(None, False)`` so the consumer skips them."""
    storage = Storage(str(tmp_path / "wt"))
    try:
        seq = storage.emit_noop_heartbeat()
        rows = list(storage.read_oplog(start_seq=seq, limit=1))
        _, entry = rows[0]
        ev, invalidates = project(
            seq,
            entry,
            storage=storage,
            scope={"kind": "cluster"},
        )
        assert ev is None
        assert invalidates is False
    finally:
        storage.close()


def test_change_stream_resume_token_advances_via_heartbeat(tmp_path) -> None:
    """End-to-end: open a change stream on a quiet collection, drive
    one explicit heartbeat, and verify the cursor's
    ``postBatchResumeToken`` advances even though no user op landed."""
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "srv"))
    srv.start()
    try:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            coll = client["heartbeat_db"]["c"]
            client["heartbeat_db"].create_collection("c")

            cs = coll.watch(max_await_time_ms=200)
            time.sleep(0.3)  # let cursor establish

            initial_token = cs.resume_token
            assert initial_token is not None

            # Fire a heartbeat directly on the storage layer.
            srv.storage.emit_noop_heartbeat()

            # tryNext drains the noop (which projects to None) and
            # refreshes ``last_token`` from the oplog row's cluster time.
            deadline = time.time() + 5
            while time.time() < deadline:
                cs.try_next()
                if cs.resume_token != initial_token:
                    break
                time.sleep(0.1)

            assert cs.resume_token != initial_token, "resume token did not advance after heartbeat"
            cs.close()
        finally:
            client.close()
    finally:
        srv.stop()


@pytest.mark.parametrize("seconds", [-1.0, 0.0])
def test_invalid_or_zero_interval_disables_heartbeat(tmp_path, seconds) -> None:
    storage = Storage(str(tmp_path / "wt"), noop_heartbeat_seconds=seconds)
    try:
        assert storage._noop_thread is None
    finally:
        storage.close()
