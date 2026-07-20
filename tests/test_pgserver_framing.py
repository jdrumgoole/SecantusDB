"""Wire-level framing / parse error handling (security review §I16).

A malformed message must produce a proper PG ``ErrorResponse`` rather than the
connection silently dropping:

* a SQL **syntax error** over the simple-query protocol returns ``42601`` and the
  connection survives (previously ``planner.parse`` ran outside the handler's
  try, so a syntax error escaped and dropped the connection);
* invalid UTF-8 in a ``Q`` message returns ``08P01`` and the connection survives
  (the message was length-framed, so the byte stream stays in sync);
* a framing error (implausible length prefix) desyncs the stream, so the server
  sends a FATAL ``08P01`` and closes instead of dropping silently.

Driven over a real socket against the real WT-backed ``Storage``.
"""

from __future__ import annotations

import socket
import struct

import pytest

from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage


def _read_until_ready(sock) -> list[pgwire.Message]:
    msgs: list[pgwire.Message] = []
    while True:
        m = pgwire.read_message(sock)
        msgs.append(m)
        if m.type == "Z":
            return msgs


@pytest.fixture
def server(tmp_path):
    storage = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=storage)  # trust mode
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        storage.close()


def _connect(server) -> socket.socket:
    host, port = server.address
    s = socket.create_connection((host, port), timeout=5)
    s.sendall(pgwire.build_startup_message({"user": "joe", "database": "db"}))
    _read_until_ready(s)
    return s


def test_syntax_error_returns_error_and_keeps_connection(server):
    s = _connect(server)
    try:
        s.sendall(pgwire.build_query("SELCT 1"))  # misspelled — a parse error
        msgs = _read_until_ready(s)
        err = next(m for m in msgs if m.type == "E")
        assert pgwire.parse_error_response(err.payload)["C"] == "42601"
        # Connection survives: a valid query still works.
        s.sendall(pgwire.build_query("SELECT 1"))
        assert any(m.type == "D" for m in _read_until_ready(s))
    finally:
        s.close()


def test_invalid_utf8_query_returns_error_and_keeps_connection(server):
    s = _connect(server)
    try:
        # A 'Q' message whose payload isn't valid UTF-8. Hand-frame it so the
        # length is correct (stream stays in sync) but parse_query fails.
        payload = b"\xff\xfe\x00"
        s.sendall(b"Q" + struct.pack("!i", len(payload) + 4) + payload)
        msgs = _read_until_ready(s)
        err = next(m for m in msgs if m.type == "E")
        assert pgwire.parse_error_response(err.payload)["C"] == "08P01"
        s.sendall(pgwire.build_query("SELECT 1"))
        assert any(m.type == "D" for m in _read_until_ready(s))
    finally:
        s.close()


def test_framing_error_sends_fatal_then_closes(server):
    s = _connect(server)
    try:
        # An implausible length (< 4) makes read_message raise PGProtocolError.
        # The server replies with a FATAL 08P01 and closes.
        s.sendall(b"Q" + struct.pack("!i", 3))
        reply = pgwire.read_message(s)
        assert reply.type == "E"
        assert pgwire.parse_error_response(reply.payload)["C"] == "08P01"
        # The server closes after the fatal error.
        with pytest.raises((pgwire.PGConnectionClosed, ConnectionError, OSError)):
            pgwire.read_message(s)
    finally:
        s.close()


def test_startup_short_cancel_packet_raises_protocol_error() -> None:
    """A CANCEL_REQUEST startup packet whose body is truncated (no pid/secret)
    makes ``struct.unpack_from`` read past the buffer. That raw ``struct.error``
    must be translated into a typed ``PGProtocolError`` — the connection loop
    turns that into a clean close, not a leaked traceback. (security review
    2026-07-20, I16.)"""

    class _FakeSock:
        def __init__(self, data: bytes) -> None:
            self._buf = bytearray(data)

        def recv(self, n: int) -> bytes:
            chunk = bytes(self._buf[:n])
            del self._buf[:n]
            return chunk

    # length=8 header + the 4-byte CANCEL code, but no pid/secret follows.
    body = struct.pack("!i", 80877102)  # CANCEL_REQUEST_CODE
    packet = struct.pack("!i", len(body) + 4) + body
    with pytest.raises(pgwire.PGProtocolError):
        pgwire.read_startup_packet(_FakeSock(packet))


def test_startup_invalid_utf8_param_raises_protocol_error() -> None:
    """A startup packet whose parameter value isn't valid UTF-8 must raise a
    typed ``PGProtocolError`` rather than a raw ``UnicodeDecodeError``.
    (security review 2026-07-20, I16.)"""

    class _FakeSock:
        def __init__(self, data: bytes) -> None:
            self._buf = bytearray(data)

        def recv(self, n: int) -> bytes:
            chunk = bytes(self._buf[:n])
            del self._buf[:n]
            return chunk

    # protocol v3 (196608) + "user\0" + an invalid-UTF-8 value + trailing NULs.
    body = struct.pack("!i", 196608) + b"user\x00\xff\xfe\x00\x00"
    packet = struct.pack("!i", len(body) + 4) + body
    with pytest.raises(pgwire.PGProtocolError):
        pgwire.read_startup_packet(_FakeSock(packet))
