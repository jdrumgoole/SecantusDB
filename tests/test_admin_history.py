"""Tests for the SQLite-backed query-history store."""

from __future__ import annotations

import pytest

from secantus.admin.history import MAX_PER_URI, HistoryStore


def test_record_then_recent(tmp_path) -> None:
    store = HistoryStore(tmp_path / "admin.db")
    store.record("mongodb://x", "find", '{"filter": {}}')
    store.record("mongodb://x", "aggregate", '{"pipeline": []}')
    rows = store.recent("mongodb://x")
    # Newest first.
    assert [r.kind for r in rows] == ["aggregate", "find"]


def test_recent_scoped_by_uri(tmp_path) -> None:
    store = HistoryStore(tmp_path / "admin.db")
    store.record("mongodb://a", "find", "1")
    store.record("mongodb://b", "find", "2")
    a = store.recent("mongodb://a")
    b = store.recent("mongodb://b")
    assert [r.payload for r in a] == ["1"]
    assert [r.payload for r in b] == ["2"]


def test_record_caps_at_max(tmp_path) -> None:
    counter = iter(range(1000))

    def fake_time() -> float:
        return float(next(counter))

    store = HistoryStore(tmp_path / "admin.db", time_func=fake_time)
    for i in range(MAX_PER_URI + 10):
        store.record("mongodb://x", "find", f"q-{i}")
    rows = store.recent("mongodb://x", limit=1000)
    assert len(rows) == MAX_PER_URI
    # The 10 oldest got pruned; the most-recent MAX_PER_URI survive.
    surviving = sorted(int(r.payload.split("-")[1]) for r in rows)
    assert surviving == list(range(10, 10 + MAX_PER_URI))


def test_record_rejects_unknown_kind(tmp_path) -> None:
    store = HistoryStore(tmp_path / "admin.db")
    with pytest.raises(ValueError):
        store.record("mongodb://x", "bogus", "")


def test_recent_limit_clamps_low(tmp_path) -> None:
    store = HistoryStore(tmp_path / "admin.db")
    store.record("mongodb://x", "find", "1")
    rows = store.recent("mongodb://x", limit=0)
    # ``limit`` is clamped to >= 1; we still get the one entry.
    assert len(rows) == 1


def test_credentialed_uri_is_scrubbed_at_rest(tmp_path) -> None:
    # A credentialed connection string must never land in the on-disk
    # admin.db in plaintext — the password is stripped to the display
    # form before it becomes a lookup key (issue #140).
    import sqlite3

    db_path = tmp_path / "admin.db"
    store = HistoryStore(db_path)
    store.record("mongodb://alice:s3cret@host:27017/?authSource=admin", "find", "1")

    raw = db_path.read_bytes()
    assert b"s3cret" not in raw

    conn = sqlite3.connect(db_path)
    try:
        stored = [row[0] for row in conn.execute("SELECT uri FROM recent_queries")]
    finally:
        conn.close()
    assert stored == ["mongodb://alice@host:27017"]


def test_scrubbed_key_still_round_trips(tmp_path) -> None:
    # record() and recent() scrub identically, so a caller passing the
    # raw credentialed URI to both still finds its own history.
    store = HistoryStore(tmp_path / "admin.db")
    uri = "mongodb://bob:pw@host/?retryWrites=true"
    store.record(uri, "find", "payload-1")
    rows = store.recent(uri)
    assert [r.payload for r in rows] == ["payload-1"]


def test_uncredentialed_uri_stored_unchanged(tmp_path) -> None:
    store = HistoryStore(tmp_path / "admin.db")
    store.record("mongodb://127.0.0.1:27017", "find", "1")
    rows = store.recent("mongodb://127.0.0.1:27017")
    assert len(rows) == 1
