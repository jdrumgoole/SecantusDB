"""Wire-protocol robustness against malformed BSON in command bodies.

Real ``mongod`` returns a ``BadValue`` error and keeps the connection
alive when a client sends an OP_MSG whose body bytes parse as a wire
frame but fail BSON validation (mismatched binary length, bad type
code, advertised doc size that overflows the buffer, etc.). The
historical SecantusDB behaviour was to let ``bson.InvalidBSON``
propagate — the connection thread crashed, the client got a
``MongoNetworkError: connection N closed`` that masked the underlying
input bug, and any in-flight cursors / sessions on that connection
went away too.

These tests build malformed bytes by hand to exercise the
``MalformedBodyError`` path in ``wire.read_message`` and the
connection loop's BadValue reply.
"""

from __future__ import annotations

import contextlib
import os
import socket
import struct
import threading

import pytest

from secantus import SecantusDBServer

# OP_MSG layout: header (16 bytes) + flags (4 bytes) + sections.
# Each kind-0 section is 1 byte (kind) + BSON document.
_HEADER_FMT = struct.Struct("<iiii")  # message_length, request_id, response_to, op_code
_OP_MSG = 2013


def _build_op_msg_with_body(request_id: int, body_bytes: bytes) -> bytes:
    """Wrap raw ``body_bytes`` in a kind-0 section with a fresh OP_MSG header."""
    flags = b"\x00\x00\x00\x00"
    section = b"\x00" + body_bytes  # kind 0 + doc
    body = flags + section
    msg_len = 16 + len(body)
    header = _HEADER_FMT.pack(msg_len, request_id, 0, _OP_MSG)
    return header + body


def _malformed_doc_bytes() -> bytes:
    """A BSON-shaped document whose declared size doesn't match its content.

    A real BSON document starts with a 4-byte little-endian int32 of the
    total size and ends with ``\\x00``. Here we declare a 30-byte doc but
    only supply 18 bytes of garbage payload, so ``bson.decode`` raises
    ``InvalidBSON: invalid length or type code``.
    """
    declared_size = struct.pack("<i", 30)
    payload = b"\x05a\x00\x05\x00\x00\x00\x00garbage"  # bogus BinData entry
    terminator = b"\x00"
    return declared_size + payload + terminator


def _read_op_msg_reply(sock: socket.socket) -> bytes:
    """Read one OP_MSG reply and return the body bytes (after header + flags)."""
    header = b""
    while len(header) < 16:
        chunk = sock.recv(16 - len(header))
        if not chunk:
            raise RuntimeError("connection closed during reply read")
        header += chunk
    msg_len = struct.unpack_from("<i", header)[0]
    body = b""
    remaining = msg_len - 16
    while remaining > 0:
        chunk = sock.recv(remaining)
        if not chunk:
            raise RuntimeError("connection closed mid-body")
        body += chunk
        remaining -= len(chunk)
    return body


def test_malformed_body_returns_bad_value_keeps_connection_open(tmp_path) -> None:
    """A handcrafted OP_MSG with invalid BSON in the body now elicits a
    targeted ``BadValue`` reply, not a dropped connection. Verified by
    sending a valid follow-up ``ping`` on the same socket."""
    import bson

    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        host, port = srv.address
        with socket.create_connection((host, port), timeout=5) as sock:
            # 1. Send malformed OP_MSG. Server should reply with BadValue.
            sock.sendall(_build_op_msg_with_body(request_id=42, body_bytes=_malformed_doc_bytes()))
            reply_body = _read_op_msg_reply(sock)
            # OP_MSG reply: flags (4 bytes) + kind-0 section (1 byte) + BSON doc.
            assert reply_body[:4] == b"\x00\x00\x00\x00", "reply flags should be 0"
            assert reply_body[4:5] == b"\x00", "reply section should be kind 0"
            doc_bytes = reply_body[5:]
            doc = bson.decode(doc_bytes)
            assert doc["ok"] == 0.0
            assert doc["code"] == 2
            assert doc["codeName"] == "BadValue"
            assert "invalid BSON" in doc["errmsg"]

            # 2. Same socket: send a valid ping. Server should respond
            # normally — proves the connection survived.
            ping_body = bson.encode({"ping": 1, "$db": "admin"})
            sock.sendall(_build_op_msg_with_body(request_id=43, body_bytes=ping_body))
            reply2 = _read_op_msg_reply(sock)
            ping_doc = bson.decode(reply2[5:])
            assert ping_doc["ok"] == 1.0


_OP_MSG_FLAG_MORE_TO_COME = 1 << 1
_OP_MSG_FLAG_EXHAUST_ALLOWED = 1 << 16


def _build_op_msg_flags(request_id: int, body_bytes: bytes, flags: int) -> bytes:
    """An OP_MSG with a kind-0 body and arbitrary flag bits (e.g. exhaustAllowed)."""
    body = struct.pack("<I", flags) + b"\x00" + body_bytes
    msg_len = 16 + len(body)
    return _HEADER_FMT.pack(msg_len, request_id, 0, _OP_MSG) + body


def _read_reply_flags_and_doc(sock: socket.socket) -> tuple[int, bytes]:
    """Read one OP_MSG reply; return its flag bits and the kind-0 BSON bytes."""
    body = _read_op_msg_reply(sock)
    flags = struct.unpack_from("<I", body)[0]
    assert body[4:5] == b"\x00", "reply section should be kind 0"
    return flags, body[5:]


def test_awaitable_exhaust_hello_streams_more_to_come(tmp_path) -> None:
    """Streaming-SDAM monitor: an awaitable ``hello`` (carries ``maxAwaitTimeMS``)
    sent with the OP_MSG ``exhaustAllowed`` flag must get a *stream* of
    ``moreToCome`` replies, and on server shutdown a final ``moreToCome``-clear
    reply that ends the stream cleanly. Without this the driver's monitor raises
    ``Server ended moreToCome unexpectedly`` on teardown and clears the pool —
    the intermittent ``mongosh`` smoke failure this guards against."""
    import bson

    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "wt"))
    srv.start()
    try:
        host, port = srv.address
        with socket.create_connection((host, port), timeout=10) as sock:
            hello = bson.encode(
                {
                    "hello": 1,
                    "maxAwaitTimeMS": 150,
                    "topologyVersion": {"processId": bson.ObjectId(), "counter": 0},
                    "$db": "admin",
                }
            )
            sock.sendall(_build_op_msg_flags(1, hello, _OP_MSG_FLAG_EXHAUST_ALLOWED))

            # First streamed reply: moreToCome set, a valid hello body.
            flags, doc_bytes = _read_reply_flags_and_doc(sock)
            assert flags & _OP_MSG_FLAG_MORE_TO_COME, "first streaming reply must set moreToCome"
            assert bson.decode(doc_bytes)["isWritablePrimary"] is True

            # A second reply arrives ~maxAwaitTimeMS later — the stream continues
            # (this is the behaviour the driver's streaming monitor requires; its
            # absence was the flake). The stream is held on its own daemon thread.
            flags2, _ = _read_reply_flags_and_doc(sock)
            assert flags2 & _OP_MSG_FLAG_MORE_TO_COME, "heartbeat reply must set moreToCome"

        # Client gone (socket closed) — like mongosh exiting. The streaming
        # thread must notice and let shutdown drain promptly rather than pinning
        # a connection thread forever.
        stopper = threading.Thread(target=srv.stop)
        stopper.start()
        stopper.join(timeout=15)
        assert not stopper.is_alive(), "stop() hung — streaming monitor thread not reaped"
    finally:
        srv.stop()


def test_awaitable_exhaust_hello_streams_when_fd_above_1024(tmp_path) -> None:
    """Regression: the awaitable-hello stream must survive a connection whose
    socket fd is >= 1024. ``select.select()`` raises ``ValueError:
    filedescriptor out of range`` past ``FD_SETSIZE`` (1024), which — under
    heavy parallel load with many open sockets — dropped the monitor connection
    after its first streamed frame (green on Windows, red on Linux/macOS CI).
    The server now waits via ``poll()``, which has no such limit. We reproduce
    the high-fd condition by exhausting the low fds first."""
    # `resource` (and the fd-value limit it guards against) is Unix-only; on
    # Windows select() isn't fd-value-bounded, so there's nothing to reproduce.
    resource = pytest.importorskip("resource")

    import bson

    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    want = 4096
    with contextlib.suppress(ValueError, OSError):
        resource.setrlimit(resource.RLIMIT_NOFILE, (min(want, hard), hard))

    hogs: list[int] = []
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "wt"))
    srv.start()
    try:
        # Burn low fds so the client socket below lands above FD_SETSIZE.
        with contextlib.suppress(OSError):
            while len(hogs) < 1100:
                hogs.append(os.open(os.devnull, os.O_RDONLY))
        host, port = srv.address
        with socket.create_connection((host, port), timeout=10) as sock:
            if sock.fileno() < 1024:
                pytest.skip(f"could not push socket fd past 1024 (got {sock.fileno()})")
            hello = bson.encode(
                {
                    "hello": 1,
                    "maxAwaitTimeMS": 100,
                    "topologyVersion": {"counter": 0},
                    "$db": "admin",
                }
            )
            sock.sendall(_build_op_msg_flags(1, hello, _OP_MSG_FLAG_EXHAUST_ALLOWED))
            # Both the first frame and the heartbeat must arrive — pre-fix the
            # second read hit a closed connection (select() raised on the high fd).
            flags1, _ = _read_reply_flags_and_doc(sock)
            flags2, _ = _read_reply_flags_and_doc(sock)
            assert flags1 & _OP_MSG_FLAG_MORE_TO_COME
            assert flags2 & _OP_MSG_FLAG_MORE_TO_COME
    finally:
        for fd in hogs:
            with contextlib.suppress(OSError):
                os.close(fd)
        srv.stop()
        with contextlib.suppress(ValueError, OSError):
            resource.setrlimit(resource.RLIMIT_NOFILE, (soft, hard))


def test_malformed_body_logs_warning_does_not_unhandled_traceback(tmp_path, caplog) -> None:
    """Pre-fix the wire path leaked a Python traceback through the
    server's catch-all handler. After the fix the bad body is logged
    at WARNING level and produces no ``unhandled error`` ERROR log."""
    import logging

    with (
        SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv,
        caplog.at_level(logging.WARNING, logger="secantus.server"),
    ):
        host, port = srv.address
        with socket.create_connection((host, port), timeout=5) as sock:
            sock.sendall(_build_op_msg_with_body(request_id=1, body_bytes=_malformed_doc_bytes()))
            # Drain the BadValue reply so the server thread finishes the round-trip.
            _read_op_msg_reply(sock)

    warnings = [r for r in caplog.records if r.levelno == logging.WARNING]
    errors = [r for r in caplog.records if r.levelno >= logging.ERROR]
    assert any("malformed BSON" in r.message for r in warnings), (
        f"expected 'malformed BSON' WARNING; got: {[r.message for r in caplog.records]}"
    )
    assert not errors, f"unexpected ERROR-level logs: {[r.message for r in errors]}"


_OP_QUERY = 2004


def _build_op_query_raw(request_id: int, op_query_body: bytes) -> bytes:
    """Wrap a raw OP_QUERY body (everything after the 16-byte header) in a
    fresh OP_QUERY frame. The body layout is flags(4) + fullCollectionName
    cstring + numberToSkip(4) + numberToReturn(4) + query BSON [+ selector]."""
    msg_len = 16 + len(op_query_body)
    return _HEADER_FMT.pack(msg_len, request_id, 0, _OP_QUERY) + op_query_body


def _assert_bad_value_then_ping_survives(sock: socket.socket, request_id: int) -> None:
    """Read a BadValue OP_MSG reply, then prove the connection survived by
    round-tripping a valid ping on the same socket."""
    import bson

    reply_body = _read_op_msg_reply(sock)
    doc = bson.decode(reply_body[5:])
    assert doc["ok"] == 0.0
    assert doc["code"] == 2
    assert doc["codeName"] == "BadValue"

    ping_body = bson.encode({"ping": 1, "$db": "admin"})
    sock.sendall(_build_op_msg_with_body(request_id=request_id, body_bytes=ping_body))
    ping_doc = bson.decode(_read_op_msg_reply(sock)[5:])
    assert ping_doc["ok"] == 1.0, "connection did not survive the malformed OP_QUERY"


def test_op_query_unterminated_collname_returns_bad_value(tmp_path) -> None:
    """A malformed OP_QUERY whose fullCollectionName has no NUL terminator
    (the issue-#116 bug) used to raise an uncaught ``ValueError`` from
    ``bytes.index`` and drop the connection without a reply. It now routes
    through the BadValue path and the connection survives."""
    # flags(4) + a collection name with NO NUL byte anywhere after it.
    body = struct.pack("<I", 0) + b"admin.$cmd-but-this-name-is-never-nul-terminated"
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        host, port = srv.address
        with socket.create_connection((host, port), timeout=5) as sock:
            sock.sendall(_build_op_query_raw(request_id=99, op_query_body=body))
            _assert_bad_value_then_ping_survives(sock, request_id=100)


def test_op_query_invalid_utf8_collname_returns_bad_value(tmp_path) -> None:
    """A NUL-terminated but non-UTF-8 collection name must not raise an
    uncaught ``UnicodeDecodeError`` (a ``ValueError`` subclass) either."""
    body = (
        struct.pack("<I", 0)
        + b"\xff\xfe\xfa"  # invalid UTF-8 collection name
        + b"\x00"
        + struct.pack("<iii", 0, 0, 5)  # skip, return, empty-doc length
        + b"\x00"  # empty BSON doc terminator
    )
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        host, port = srv.address
        with socket.create_connection((host, port), timeout=5) as sock:
            sock.sendall(_build_op_query_raw(request_id=7, op_query_body=body))
            _assert_bad_value_then_ping_survives(sock, request_id=8)


def test_op_query_truncated_after_collname_returns_bad_value(tmp_path) -> None:
    """An OP_QUERY truncated before the skip/return/query fields must surface
    BadValue (a ``struct.error`` from ``unpack_from``), not drop the socket."""
    body = struct.pack("<I", 0) + b"db.coll" + b"\x00" + b"\x01\x02"  # truncated
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        host, port = srv.address
        with socket.create_connection((host, port), timeout=5) as sock:
            sock.sendall(_build_op_query_raw(request_id=11, op_query_body=body))
            _assert_bad_value_then_ping_survives(sock, request_id=12)


def test_op_query_negative_doc_len_returns_bad_value(tmp_path) -> None:
    """A negative declared query-doc length must be caught by ``_check_doc_len``
    (the OP_MSG hardening, now applied to OP_QUERY too), not produce a garbage
    slice that ``bson.decode`` crashes the connection thread on."""
    body = (
        struct.pack("<I", 0)
        + b"db.coll"
        + b"\x00"
        + struct.pack("<iii", 0, 0, -1)  # skip, return, doc_len = -1
        + b"\x00\x00\x00\x00"
    )
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
        host, port = srv.address
        with socket.create_connection((host, port), timeout=5) as sock:
            sock.sendall(_build_op_query_raw(request_id=13, op_query_body=body))
            _assert_bad_value_then_ping_survives(sock, request_id=14)


def test_op_query_malformed_logs_warning_not_traceback(tmp_path, caplog) -> None:
    """The malformed OP_QUERY must be a WARNING, never an ``unhandled error``
    ERROR-level traceback through the catch-all handler."""
    import logging

    body = struct.pack("<I", 0) + b"no-nul-here-at-all-not-even-once"
    with (
        SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv,
        caplog.at_level(logging.WARNING, logger="secantus.server"),
    ):
        host, port = srv.address
        with socket.create_connection((host, port), timeout=5) as sock:
            sock.sendall(_build_op_query_raw(request_id=1, op_query_body=body))
            _read_op_msg_reply(sock)  # drain the BadValue reply

    errors = [r for r in caplog.records if r.levelno >= logging.ERROR]
    assert not errors, f"unexpected ERROR-level logs: {[r.message for r in errors]}"
    assert any(
        "malformed BSON" in r.message for r in caplog.records if r.levelno == logging.WARNING
    )


def test_abrupt_reset_close_is_quiet(tmp_path, caplog) -> None:
    """An RST-style hang-up (SO_LINGER 0 close — how Go-driver tools like
    mongodump drop pooled connections) is a normal disconnect: DEBUG log,
    no ``unhandled error`` traceback through the catch-all handler."""
    import logging
    import time

    import bson

    with (
        SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv,
        caplog.at_level(logging.DEBUG, logger="secantus.server"),
    ):
        host, port = srv.address
        sock = socket.create_connection((host, port), timeout=5)
        # Complete one round-trip so the server is parked in read_message
        # waiting for the next request when the reset lands.
        ping_body = bson.encode({"ping": 1, "$db": "admin"})
        sock.sendall(_build_op_msg_with_body(request_id=1, body_bytes=ping_body))
        _read_op_msg_reply(sock)
        # SO_LINGER(on, 0) makes close() send RST instead of FIN.
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        sock.close()
        # Wait until the server reaps the connection (registry empties)
        # rather than sleeping a fixed interval.
        deadline = time.monotonic() + 5
        while srv.connections.snapshot() and time.monotonic() < deadline:
            time.sleep(0.01)
        assert not srv.connections.snapshot(), "server never reaped the reset connection"

    errors = [r for r in caplog.records if r.levelno >= logging.ERROR]
    assert not errors, f"unexpected ERROR-level logs: {[r.message for r in errors]}"


def test_recursion_error_in_parse_becomes_bad_value(monkeypatch) -> None:
    """A ``RecursionError`` raised while parsing an OP_MSG body (a
    pathologically deeply-nested BSON document exhausting the recursion limit)
    is translated to ``MalformedBodyError`` — the same BadValue path as
    ``bson.InvalidBSON`` — so the connection survives rather than the
    ``RecursionError`` escaping the parse. (security review 2026-07-20, I1.)

    At the default recursion limit a deeply-nested document is already rejected
    as ``InvalidBSON`` by pymongo's decoder; this pins the defensive branch that
    covers a ``RecursionError`` escaping regardless.
    """
    import bson

    from secantus import wire

    class _FakeSock:
        def __init__(self, data: bytes) -> None:
            self._buf = bytearray(data)

        def recv(self, n: int) -> bytes:
            chunk = bytes(self._buf[:n])
            del self._buf[:n]
            return chunk

    monkeypatch.setattr(wire, "_parse_op_msg", lambda body: (_ for _ in ()).throw(RecursionError()))
    frame = _build_op_msg_with_body(request_id=7, body_bytes=bson.encode({"ping": 1}))
    with pytest.raises(wire.MalformedBodyError) as ei:
        wire.read_message(_FakeSock(frame))
    assert "nesting too deep" in str(ei.value)
