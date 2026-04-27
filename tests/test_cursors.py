from __future__ import annotations

import pytest

from secantus.cursors import CursorNotFound, CursorRegistry


def test_register_returns_unique_increasing_ids() -> None:
    reg = CursorRegistry()
    a = reg.register("db.c", [{"x": 1}])
    b = reg.register("db.c", [{"x": 2}])
    assert a != b
    assert b > a


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
