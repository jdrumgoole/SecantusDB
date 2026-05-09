from __future__ import annotations

import pytest

from secantus.cursors import CursorNotFound, CursorRegistry


def test_register_returns_unique_ids() -> None:
    reg = CursorRegistry()
    # IDs are random 63-bit ints, not sequential — ordering is not
    # guaranteed (and was previously a guessability hazard).
    a = reg.register("db.c", [{"x": 1}])
    b = reg.register("db.c", [{"x": 2}])
    assert a != b
    assert a > 0 and b > 0


def test_next_batch_returns_chunks_then_exhausts() -> None:
    reg = CursorRegistry()
    cid = reg.register("db.c", [{"i": i} for i in range(5)])

    batch, exhausted = reg.next_batch(cid, 3)
    assert [d["i"] for d in batch] == [0, 1, 2]
    assert not exhausted

    batch, exhausted = reg.next_batch(cid, 3)
    assert [d["i"] for d in batch] == [3, 4]
    assert exhausted


def test_cursor_removed_after_exhaustion() -> None:
    reg = CursorRegistry()
    cid = reg.register("db.c", [{"x": 1}])
    reg.next_batch(cid, 10)
    with pytest.raises(CursorNotFound):
        reg.next_batch(cid, 10)


def test_zero_batch_size_returns_all_remaining() -> None:
    reg = CursorRegistry()
    cid = reg.register("db.c", [{"i": i} for i in range(3)])
    batch, exhausted = reg.next_batch(cid, 0)
    assert len(batch) == 3
    assert exhausted


def test_kill_removes_existing_and_reports_missing() -> None:
    reg = CursorRegistry()
    a = reg.register("db.c", [{"x": 1}])
    b = reg.register("db.c", [{"x": 2}])
    killed, not_found = reg.kill([a, 99999, b])
    assert sorted(killed) == sorted([a, b])
    assert not_found == [99999]
    assert len(reg) == 0


def test_unknown_cursor_raises_not_found() -> None:
    reg = CursorRegistry()
    with pytest.raises(CursorNotFound):
        reg.next_batch(12345, 10)


# ---- TTL expiry: idle cursors are pruned and surface as CursorNotFound. ----


class _ManualClock:
    def __init__(self, t: float = 0.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t

    def advance(self, dt: float) -> None:
        self.t += dt


def test_default_ttl_is_ten_minutes() -> None:
    reg = CursorRegistry()
    assert reg.idle_ttl_seconds == 600.0


def test_idle_cursor_expires_after_ttl() -> None:
    clock = _ManualClock()
    reg = CursorRegistry(idle_ttl_seconds=60.0, time_func=clock)
    cid = reg.register("db.c", [{"i": i} for i in range(3)])
    clock.advance(61.0)
    with pytest.raises(CursorNotFound):
        reg.next_batch(cid, 10)


def test_active_cursor_does_not_expire_while_used() -> None:
    clock = _ManualClock()
    reg = CursorRegistry(idle_ttl_seconds=60.0, time_func=clock)
    cid = reg.register("db.c", [{"i": i} for i in range(10)])
    for _ in range(5):
        clock.advance(30.0)
        reg.next_batch(cid, 1)
    # Total elapsed: 150s, but each idle gap was only 30s — well under TTL.
    batch, _ = reg.next_batch(cid, 5)
    assert len(batch) == 5


def test_expiry_purges_from_registry_len() -> None:
    clock = _ManualClock()
    reg = CursorRegistry(idle_ttl_seconds=10.0, time_func=clock)
    reg.register("db.c", [{"x": 1}])
    reg.register("db.c", [{"x": 2}])
    assert len(reg) == 2
    clock.advance(11.0)
    # Touch the registry — opportunistic prune fires.
    reg.register("db.c", [{"x": 3}])
    assert len(reg) == 1


def test_killed_cursor_idempotent_after_expiry() -> None:
    clock = _ManualClock()
    reg = CursorRegistry(idle_ttl_seconds=10.0, time_func=clock)
    cid = reg.register("db.c", [{"x": 1}])
    clock.advance(11.0)
    killed, not_found = reg.kill([cid])
    # Already expired → reported as not-found, not killed.
    assert killed == []
    assert not_found == [cid]


def test_snapshot_returns_metadata_for_live_cursors() -> None:
    reg = CursorRegistry()
    a = reg.register("db.c", [{"i": i} for i in range(5)])
    b = reg.register("db.other", [{"i": 99}])
    snap = reg.snapshot()
    assert {s["cursor_id"] for s in snap} == {a, b}
    a_entry = next(s for s in snap if s["cursor_id"] == a)
    assert a_entry["namespace"] == "db.c"
    assert a_entry["remaining"] == 5
    assert a_entry["tailable"] is False


def test_snapshot_drops_exhausted_cursors() -> None:
    reg = CursorRegistry()
    cid = reg.register("db.c", [{"x": 1}])
    reg.next_batch(cid, 10)  # exhausts
    assert reg.snapshot() == []
