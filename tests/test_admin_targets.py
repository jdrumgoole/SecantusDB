"""Target-URI store unit tests."""

from __future__ import annotations

from secantus.admin.targets import MAX_TARGETS, TargetStore


def test_record_then_recent(tmp_path) -> None:
    store = TargetStore(tmp_path / "admin.db")
    store.record("mongodb://a")
    store.record("mongodb://b")
    rows = store.recent()
    assert [r.uri for r in rows] == ["mongodb://b", "mongodb://a"]


def test_record_same_uri_updates_last_used(tmp_path) -> None:
    counter = iter(range(100))

    def fake_time() -> float:
        return float(next(counter))

    store = TargetStore(tmp_path / "admin.db", time_func=fake_time)
    store.record("mongodb://a")  # ts 0
    store.record("mongodb://b")  # ts 1
    store.record("mongodb://a")  # ts 2 — re-touched
    rows = store.recent()
    # ``a`` is now the most-recent because its last_used was bumped.
    assert [r.uri for r in rows] == ["mongodb://a", "mongodb://b"]


def test_forget_removes_uri(tmp_path) -> None:
    store = TargetStore(tmp_path / "admin.db")
    store.record("mongodb://a")
    store.record("mongodb://b")
    store.forget("mongodb://a")
    rows = store.recent()
    assert [r.uri for r in rows] == ["mongodb://b"]


def test_record_ignores_empty(tmp_path) -> None:
    store = TargetStore(tmp_path / "admin.db")
    store.record("")
    store.record("   ")
    assert store.recent() == []


def test_record_caps_at_max(tmp_path) -> None:
    counter = iter(range(1000))

    def fake_time() -> float:
        return float(next(counter))

    store = TargetStore(tmp_path / "admin.db", time_func=fake_time)
    for i in range(MAX_TARGETS + 5):
        store.record(f"mongodb://host-{i}")
    rows = store.recent(limit=1000)
    assert len(rows) == MAX_TARGETS
    # The 5 oldest got pruned.
    survived = sorted(int(r.uri.rsplit("-", 1)[1]) for r in rows)
    assert survived == list(range(5, 5 + MAX_TARGETS))


def test_recent_order_stable_when_timestamps_tie(tmp_path) -> None:
    """Windows' ~15.6ms time.time() resolution makes back-to-back records
    tie on last_used_at; the rowid tiebreaker must keep most-recent-first
    deterministic (this flaked on the Windows CI lane before the fix)."""
    store = TargetStore(tmp_path / "admin.db", time_func=lambda: 1000.0)
    store.record("mongodb://a")
    store.record("mongodb://b")
    store.record("mongodb://c")
    assert [r.uri for r in store.recent()] == [
        "mongodb://c",
        "mongodb://b",
        "mongodb://a",
    ]
