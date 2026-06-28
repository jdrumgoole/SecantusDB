"""PostgreSQL v3 frontend/backend wire protocol — framing + messages.

The SQL analogue of ``secantus.wire``: it knows the byte layout of the protocol
and nothing about SQL semantics. Layout recap (all integers big-endian):

- Most messages: 1 type byte, Int32 length (counts itself, excludes the type
  byte), then payload.
- The first message of a connection (``StartupMessage`` / ``SSLRequest`` /
  ``CancelRequest`` / ``GSSENCRequest``) has *no* type byte: Int32 length, Int32
  code/version, then payload.

P1 implements the subset ``psql``'s simple-query path needs: startup +
``AuthenticationOk`` (trust) + ``ParameterStatus`` + ``BackendKeyData`` +
``ReadyForQuery``, then ``Query`` → ``RowDescription`` / ``DataRow`` /
``CommandComplete`` / ``ErrorResponse``. The extended query protocol
(``Parse``/``Bind``/``Execute``) is a later phase.
"""

from __future__ import annotations

import socket
import struct
from dataclasses import dataclass

_INT32 = struct.Struct("!i")
_INT16 = struct.Struct("!h")

# Magic codes carried in the first Int32 of a startup-class packet.
PROTOCOL_VERSION_3 = 196608  # 3.0 << 16
SSL_REQUEST_CODE = 80877103
GSSENC_REQUEST_CODE = 80877104
CANCEL_REQUEST_CODE = 80877102


class PGProtocolError(Exception):
    pass


class PGConnectionClosed(Exception):
    pass


# --------------------------------------------------------------------------- #
# Low-level reads
# --------------------------------------------------------------------------- #


def recv_exactly(sock: socket.socket, n: int) -> bytes:
    chunks: list[bytes] = []
    remaining = n
    while remaining > 0:
        chunk = sock.recv(remaining)
        if not chunk:
            raise PGConnectionClosed
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


@dataclass
class StartupMessage:
    params: dict[str, str]


@dataclass
class SSLRequest:
    pass


@dataclass
class GSSENCRequest:
    pass


@dataclass
class CancelRequest:
    pid: int
    secret: int


StartupPacket = StartupMessage | SSLRequest | GSSENCRequest | CancelRequest


def read_startup_packet(sock: socket.socket) -> StartupPacket:
    """Read the connection's first (type-byte-less) packet."""
    header = recv_exactly(sock, 4)
    (length,) = _INT32.unpack(header)
    if length < 8 or length > 10_000_000:
        raise PGProtocolError(f"implausible startup length {length}")
    body = recv_exactly(sock, length - 4)
    (code,) = _INT32.unpack_from(body, 0)
    if code == SSL_REQUEST_CODE:
        return SSLRequest()
    if code == GSSENC_REQUEST_CODE:
        return GSSENCRequest()
    if code == CANCEL_REQUEST_CODE:
        pid, secret = struct.unpack_from("!ii", body, 4)
        return CancelRequest(pid=pid, secret=secret)
    if code != PROTOCOL_VERSION_3:
        raise PGProtocolError(f"unsupported protocol/startup code {code}")
    # Remainder is a run of NUL-terminated key/value strings, ending with an
    # empty key (a lone NUL).
    params: dict[str, str] = {}
    parts = body[4:].split(b"\x00")
    i = 0
    while i + 1 < len(parts):
        key = parts[i].decode("utf-8")
        if key == "":
            break
        params[key] = parts[i + 1].decode("utf-8")
        i += 2
    return StartupMessage(params=params)


@dataclass
class Message:
    type: str  # single-char message type
    payload: bytes


def read_message(sock: socket.socket) -> Message:
    """Read one typed frontend message (type byte + length + payload)."""
    type_byte = recv_exactly(sock, 1)
    (length,) = _INT32.unpack(recv_exactly(sock, 4))
    if length < 4 or length > 100_000_000:
        raise PGProtocolError(f"implausible message length {length}")
    payload = recv_exactly(sock, length - 4) if length > 4 else b""
    return Message(type=type_byte.decode("latin-1"), payload=payload)


def parse_query(payload: bytes) -> str:
    """Extract the SQL string from a 'Q' (Query) message payload."""
    return payload.split(b"\x00", 1)[0].decode("utf-8")


# --------------------------------------------------------------------------- #
# Backend message builders
# --------------------------------------------------------------------------- #


def _msg(type_byte: str, payload: bytes) -> bytes:
    return type_byte.encode("latin-1") + _INT32.pack(len(payload) + 4) + payload


def _cstr(s: str) -> bytes:
    return s.encode("utf-8") + b"\x00"


def authentication_ok() -> bytes:
    return _msg("R", _INT32.pack(0))


def parameter_status(name: str, value: str) -> bytes:
    return _msg("S", _cstr(name) + _cstr(value))


def backend_key_data(pid: int, secret: int) -> bytes:
    return _msg("K", struct.pack("!ii", pid, secret))


def ready_for_query(status: bytes = b"I") -> bytes:
    return _msg("Z", status)


def row_description(columns: list[tuple[str, int]]) -> bytes:
    """``columns`` is a list of (name, type_oid). Text format, sizes left -1."""
    payload = bytearray(_INT16.pack(len(columns)))
    for name, type_oid in columns:
        payload += _cstr(name)
        payload += _INT32.pack(0)  # table OID (unknown)
        payload += _INT16.pack(0)  # column attribute number
        payload += _INT32.pack(type_oid)
        payload += _INT16.pack(-1)  # type size (variable)
        payload += _INT32.pack(-1)  # type modifier
        payload += _INT16.pack(0)  # format code: text
    return _msg("T", bytes(payload))


def data_row(values: list[bytes | None]) -> bytes:
    payload = bytearray(_INT16.pack(len(values)))
    for v in values:
        if v is None:
            payload += _INT32.pack(-1)
        else:
            payload += _INT32.pack(len(v)) + v
    return _msg("D", bytes(payload))


def command_complete(tag: str) -> bytes:
    return _msg("C", _cstr(tag))


def empty_query_response() -> bytes:
    return _msg("I", b"")


def error_response(sqlstate: str, message: str, severity: str = "ERROR") -> bytes:
    # ErrorResponse is a sequence of (field-type byte, value cstring), ending
    # with a single NUL. S/V severity, C SQLSTATE, M human message are the
    # minimum libpq surfaces.
    payload = (
        b"S" + _cstr(severity)
        + b"V" + _cstr(severity)
        + b"C" + _cstr(sqlstate)
        + b"M" + _cstr(message)
        + b"\x00"
    )
    return _msg("E", payload)


def notice_response(message: str) -> bytes:
    payload = b"S" + _cstr("NOTICE") + b"M" + _cstr(message) + b"\x00"
    return _msg("N", payload)


def build_startup_message(params: dict[str, str]) -> bytes:
    """Client-side helper (used by tests): assemble a StartupMessage."""
    body = bytearray(_INT32.pack(PROTOCOL_VERSION_3))
    for k, v in params.items():
        body += _cstr(k) + _cstr(v)
    body += b"\x00"
    return _INT32.pack(len(body) + 4) + bytes(body)


def build_query(sql: str) -> bytes:
    """Client-side helper (used by tests): assemble a 'Q' Query message."""
    return _msg("Q", _cstr(sql))


def build_terminate() -> bytes:
    return _msg("X", b"")


def parse_row_description(payload: bytes) -> list[str]:
    """Client-side helper: column names out of a 'T' message payload."""
    (count,) = _INT16.unpack_from(payload, 0)
    names: list[str] = []
    offset = 2
    for _ in range(count):
        end = payload.index(b"\x00", offset)
        names.append(payload[offset:end].decode("utf-8"))
        offset = end + 1 + 18  # skip the 18 fixed bytes after the name
    return names


def parse_data_row(payload: bytes) -> list[bytes | None]:
    """Client-side helper: column values out of a 'D' message payload."""
    (count,) = _INT16.unpack_from(payload, 0)
    values: list[bytes | None] = []
    offset = 2
    for _ in range(count):
        (length,) = _INT32.unpack_from(payload, offset)
        offset += 4
        if length == -1:
            values.append(None)
        else:
            values.append(payload[offset : offset + length])
            offset += length
    return values


def parse_error_response(payload: bytes) -> dict[str, str]:
    """Client-side helper: field map out of an 'E' message payload."""
    fields: dict[str, str] = {}
    for chunk in payload.split(b"\x00"):
        if not chunk:
            continue
        fields[chr(chunk[0])] = chunk[1:].decode("utf-8", "replace")
    return fields


def parse_command_complete(payload: bytes) -> str:
    return payload.split(b"\x00", 1)[0].decode("utf-8")
