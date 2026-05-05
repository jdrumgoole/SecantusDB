from __future__ import annotations

import pytest

from secantus.logbuf import LogBuffer


def test_append_then_tail_returns_entries_oldest_first() -> None:
    buf = LogBuffer(capacity=10)
    buf.append("I", "NETWORK", "first")
    buf.append("W", "STORAGE", "second")
    entries = buf.tail()
    assert [e.msg for e in entries] == ["first", "second"]
    assert entries[0].level == "I"
    assert entries[1].component == "STORAGE"


def test_tail_n_returns_most_recent() -> None:
    buf = LogBuffer(capacity=10)
    for i in range(5):
        buf.append("I", "C", f"msg-{i}")
    last_two = buf.tail(2)
    assert [e.msg for e in last_two] == ["msg-3", "msg-4"]


def test_tail_n_clamps_to_buffer_size() -> None:
    buf = LogBuffer(capacity=10)
    buf.append("I", "C", "only")
    assert len(buf.tail(100)) == 1


def test_capacity_drops_oldest() -> None:
    buf = LogBuffer(capacity=3)
    for i in range(5):
        buf.append("I", "C", f"msg-{i}")
    msgs = [e.msg for e in buf.tail()]
    assert msgs == ["msg-2", "msg-3", "msg-4"]


def test_capacity_must_be_positive() -> None:
    with pytest.raises(ValueError):
        LogBuffer(capacity=0)
    with pytest.raises(ValueError):
        LogBuffer(capacity=-1)


def test_ctx_payload_round_trips() -> None:
    buf = LogBuffer(capacity=10)
    buf.append("I", "NETWORK", "connect", {"conn_id": 42, "from": ("127.0.0.1", 5000)})
    [entry] = buf.tail()
    assert entry.ctx == {"conn_id": 42, "from": ("127.0.0.1", 5000)}
