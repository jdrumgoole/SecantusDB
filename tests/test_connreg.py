from __future__ import annotations

import threading

from secantus.connreg import ConnectionRegistry


def test_open_assigns_monotonic_ids() -> None:
    reg = ConnectionRegistry()
    a = reg.open(("127.0.0.1", 1001))
    b = reg.open(("127.0.0.1", 1002))
    assert a == 1
    assert b == 2


def test_record_command_updates_counters() -> None:
    reg = ConnectionRegistry()
    cid = reg.open(("127.0.0.1", 1001))
    reg.record_command(cid, "find")
    reg.record_command(cid, "insert")
    info = next(i for i in reg.snapshot() if i.conn_id == cid)
    assert info.op_count == 2
    assert info.last_command_name == "insert"
    assert info.last_cmd_at is not None


def test_authenticate_sets_user() -> None:
    reg = ConnectionRegistry()
    cid = reg.open(("127.0.0.1", 1001))
    reg.authenticate(cid, "alice@admin")
    info = next(i for i in reg.snapshot() if i.conn_id == cid)
    assert info.user == "alice@admin"


def test_close_removes_from_snapshot() -> None:
    reg = ConnectionRegistry()
    a = reg.open(("127.0.0.1", 1001))
    b = reg.open(("127.0.0.1", 1002))
    reg.close(a)
    ids = [i.conn_id for i in reg.snapshot()]
    assert ids == [b]


def test_snapshot_is_isolated_copy() -> None:
    reg = ConnectionRegistry()
    reg.open(("127.0.0.1", 1001))
    snap = reg.snapshot()
    snap[0].op_count = 999
    fresh = reg.snapshot()
    assert fresh[0].op_count == 0


def test_record_command_on_unknown_conn_is_safe() -> None:
    reg = ConnectionRegistry()
    # Should not raise, just no-op.
    reg.record_command(99999, "find")
    reg.authenticate(99999, "alice@admin")


def test_concurrent_open_close_is_safe() -> None:
    reg = ConnectionRegistry()
    ids: list[int] = []

    def worker() -> None:
        for _ in range(50):
            cid = reg.open(("127.0.0.1", 1234))
            ids.append(cid)
            reg.record_command(cid, "find")
            reg.close(cid)

    threads = [threading.Thread(target=worker) for _ in range(8)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    assert len(ids) == 400
    assert len(set(ids)) == 400  # all unique
    assert len(reg) == 0


def test_len_reflects_open_count() -> None:
    reg = ConnectionRegistry()
    assert len(reg) == 0
    reg.open(("127.0.0.1", 1001))
    reg.open(("127.0.0.1", 1002))
    assert len(reg) == 2
