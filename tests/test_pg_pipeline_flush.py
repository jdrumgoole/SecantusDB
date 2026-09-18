"""Extended-protocol replies are held until Sync or Flush, as PostgreSQL holds them.

The Python server used to send every reply the moment it produced it. A
PostgreSQL backend does not: it flushes at ReadyForQuery (after Sync), on an
explicit Flush, or when its 8 KB send buffer fills ("Flush ... forces the
backend to deliver any data pending in its output buffers"). psycopg's
``test_pipeline_async.py::test_executemany_trace`` counts client/server round
trips and expects exactly one for a single-Sync pipeline; an early
ParseComplete split it into two whenever the reply raced the client's remaining
writes, so the gauge failed on some runs and passed on others.

Wire level on purpose: psycopg's own trace fixture is unavailable on Windows,
and this pins the cause rather than one client's view of it.
"""

from __future__ import annotations

import select
import socket
import struct

import pytest

from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage


def _msg(kind: bytes, body: bytes = b"") -> bytes:
    return kind + struct.pack("!i", len(body) + 4) + body


def _cstr(s: str) -> bytes:
    return s.encode() + b"\x00"


PARSE = _msg(b"P", _cstr("") + _cstr("select 1") + struct.pack("!h", 0))
BIND = _msg(b"B", _cstr("") + _cstr("") + struct.pack("!hhh", 0, 0, 0))
EXECUTE = _msg(b"E", _cstr("") + struct.pack("!i", 0))
SYNC = _msg(b"S")
FLUSH = _msg(b"H")


@pytest.fixture
def sock(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        s = socket.create_connection((host, port), timeout=10)
        try:
            s.sendall(pgwire.build_startup_message({"user": "postgres", "database": "postgres"}))
            while pgwire.read_message(s).type != "Z":
                pass
            yield s
        finally:
            s.close()
    finally:
        srv.stop()
        st.close()


def _nothing_arrives(s: socket.socket, wait: float = 0.5) -> bool:
    ready, _, _ = select.select([s], [], [], wait)
    return not ready


def _read_types(s: socket.socket, until: str, limit: int = 20) -> list[str]:
    out: list[str] = []
    for _ in range(limit):
        m = pgwire.read_message(s)
        out.append(m.type)
        if m.type == until:
            return out
    raise AssertionError(f"no {until!r} within {limit} messages: {out}")


def test_nothing_is_sent_before_sync(sock) -> None:
    sock.sendall(PARSE + BIND + EXECUTE)
    assert _nothing_arrives(sock), "a reply was sent before Sync / Flush"
    sock.sendall(SYNC)
    assert _read_types(sock, "Z") == ["1", "2", "D", "C", "Z"]


def test_flush_delivers_without_sync(sock) -> None:
    """The other half of the contract: without it, a Flush-driven client hangs."""
    sock.sendall(PARSE + BIND + EXECUTE + FLUSH)
    assert _read_types(sock, "C") == ["1", "2", "D", "C"]
    sock.sendall(SYNC)
    assert _read_types(sock, "Z") == ["Z"]


def test_an_error_is_not_held_back(sock) -> None:
    # Bind to a statement that was never prepared: 26000, raised by Bind itself.
    # (Not a syntax error in a Parse: this server defers that to Execute, which
    # Postgres does not -- tracked in tasks/backlog.md.)
    bad = _msg(b"B", _cstr("") + _cstr("never_prepared") + struct.pack("!hhh", 0, 0, 0))
    sock.sendall(bad)
    assert _read_types(sock, "E") == ["E"]
    sock.sendall(SYNC)
    assert _read_types(sock, "Z") == ["Z"]


def test_a_large_result_streams_before_sync(sock) -> None:
    """Past PG's 8 KB send buffer the backend flushes early, so a big result is
    not held whole in memory until Sync."""
    big = _msg(b"P", _cstr("") + _cstr("select repeat('x', 20000)") + struct.pack("!h", 0))
    sock.sendall(big + BIND + EXECUTE)
    assert not _nothing_arrives(sock, wait=2.0), "a >8 KB result waited for Sync"
    sock.sendall(SYNC)
    assert _read_types(sock, "Z")[-1] == "Z"
