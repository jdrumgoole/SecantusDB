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

import socket
import struct

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
