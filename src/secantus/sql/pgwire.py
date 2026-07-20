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
    # Every read below is on attacker-controlled bytes. A malformed startup
    # packet must raise ``PGProtocolError`` (which the connection loop turns
    # into a clean close, not a leaked traceback), never a raw ``struct.error``
    # / ``UnicodeDecodeError``. Mirrors ``wire.py``'s BSON-framing discipline.
    # (security review 2026-07-20, I16.)
    try:
        (code,) = _INT32.unpack_from(body, 0)
        if code == SSL_REQUEST_CODE:
            return SSLRequest()
        if code == GSSENC_REQUEST_CODE:
            return GSSENCRequest()
        if code == CANCEL_REQUEST_CODE:
            # A truncated CANCEL body (fewer than 12 bytes) makes
            # ``unpack_from`` read past the buffer → ``struct.error``.
            pid, secret = struct.unpack_from("!ii", body, 4)
            return CancelRequest(pid=pid, secret=secret)
        if code != PROTOCOL_VERSION_3:
            raise PGProtocolError(f"unsupported protocol/startup code {code}")
        # Remainder is a run of NUL-terminated key/value strings, ending with
        # an empty key (a lone NUL). A key or value that isn't valid UTF-8
        # raises ``UnicodeDecodeError``.
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
    except (struct.error, UnicodeDecodeError) as exc:
        raise PGProtocolError(f"malformed startup packet: {exc}") from exc


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


def decode_text(raw: bytes, encoding: str | None) -> str:
    """Decode client-sent text per the connection's ``client_encoding``.

    ``None`` (SQL_ASCII) performs no conversion — utf-8 + surrogateescape makes
    arbitrary byte sequences round-trip losslessly through Python str."""
    if encoding is None:
        return raw.decode("utf-8", "surrogateescape")
    return raw.decode(encoding)


def encode_text(text: str, encoding: str | None, *, lossy: bool = False) -> bytes:
    """Encode server text per the connection's ``client_encoding`` (inverse of
    ``decode_text``).

    Data conversion is strict: a character with no equivalent in the client
    encoding raises 22P05, as Postgres does. ``lossy=True`` (error messages —
    which must never raise while being delivered) degrades to ``?``."""
    if encoding is None:
        return text.encode("utf-8", "surrogateescape")
    if lossy:
        return text.encode(encoding, "replace")
    try:
        return text.encode(encoding)
    except UnicodeEncodeError as exc:
        from secantus.sql import errors as _errors

        ch = text[exc.start]
        raise _errors.SQLError(
            "22P05",
            f"character with byte sequence {ch.encode('utf-8').hex()} in encoding "
            f'"UTF8" has no equivalent in the client encoding',
        ) from exc


def transcode_out(value: bytes | None, encoding: str | None) -> bytes | None:
    """Re-encode a utf-8 result value for the client's encoding. The engine is
    utf-8 throughout; this runs only at the wire boundary, and only when the
    client asked for something else."""
    if value is None or encoding is None or encoding == "utf-8":
        return value
    return encode_text(value.decode("utf-8", "surrogateescape"), encoding)


def parse_query(payload: bytes, encoding: str | None = "utf-8") -> str:
    """Extract the SQL string from a 'Q' (Query) message payload."""
    return decode_text(payload.split(b"\x00", 1)[0], encoding)


# --------------------------------------------------------------------------- #
# Extended-protocol frontend parsers
# --------------------------------------------------------------------------- #


def _read_cstr(payload: bytes, offset: int) -> tuple[str, int]:
    end = payload.index(b"\x00", offset)
    return payload[offset:end].decode("utf-8"), end + 1


def parse_parse(payload: bytes, encoding: str | None = "utf-8") -> tuple[str, str, list[int]]:
    """'P' Parse: (statement_name, query, param_type_oids)."""
    name, offset = _read_cstr(payload, 0)
    end = payload.index(b"\x00", offset)
    query = decode_text(payload[offset:end], encoding)
    offset = end + 1
    (n,) = _INT16.unpack_from(payload, offset)
    offset += 2
    oids = [_INT32.unpack_from(payload, offset + 4 * i)[0] for i in range(n)]
    return name, query, oids


def parse_bind(payload: bytes) -> tuple[str, str, list[int], list[bytes | None], list[int]]:
    """'B' Bind: (portal, statement, param_format_codes, param_values, result_formats)."""
    portal, offset = _read_cstr(payload, 0)
    statement, offset = _read_cstr(payload, offset)
    (n_fmt,) = _INT16.unpack_from(payload, offset)
    offset += 2
    formats = [_INT16.unpack_from(payload, offset + 2 * i)[0] for i in range(n_fmt)]
    offset += 2 * n_fmt
    (n_params,) = _INT16.unpack_from(payload, offset)
    offset += 2
    values: list[bytes | None] = []
    for _ in range(n_params):
        (length,) = _INT32.unpack_from(payload, offset)
        offset += 4
        if length == -1:
            values.append(None)
        else:
            values.append(payload[offset : offset + length])
            offset += length
    (n_res,) = _INT16.unpack_from(payload, offset)
    offset += 2
    result_formats = [_INT16.unpack_from(payload, offset + 2 * i)[0] for i in range(n_res)]
    return portal, statement, formats, values, result_formats


def parse_describe(payload: bytes) -> tuple[str, str]:
    """'D' Describe: (kind 'S'|'P', name)."""
    kind = chr(payload[0])
    name, _ = _read_cstr(payload, 1)
    return kind, name


def parse_execute(payload: bytes) -> tuple[str, int]:
    """'E' Execute: (portal, max_rows)."""
    portal, offset = _read_cstr(payload, 0)
    (max_rows,) = _INT32.unpack_from(payload, offset)
    return portal, max_rows


def parse_close(payload: bytes) -> tuple[str, str]:
    """'C' Close: (kind 'S'|'P', name)."""
    kind = chr(payload[0])
    name, _ = _read_cstr(payload, 1)
    return kind, name


# --------------------------------------------------------------------------- #
# Backend message builders
# --------------------------------------------------------------------------- #


def _msg(type_byte: str, payload: bytes) -> bytes:
    return type_byte.encode("latin-1") + _INT32.pack(len(payload) + 4) + payload


def _cstr(s: str) -> bytes:
    return s.encode("utf-8") + b"\x00"


def authentication_ok() -> bytes:
    return _msg("R", _INT32.pack(0))


def authentication_sasl(mechanisms: list[str]) -> bytes:
    """'R'(10) AuthenticationSASL — advertise the SASL mechanisms."""
    payload = bytearray(_INT32.pack(10))
    for mech in mechanisms:
        payload += _cstr(mech)
    payload += b"\x00"  # final empty string terminates the list
    return _msg("R", bytes(payload))


def authentication_sasl_continue(data: bytes) -> bytes:
    """'R'(11) AuthenticationSASLContinue — the SCRAM server-first message."""
    return _msg("R", _INT32.pack(11) + data)


def authentication_sasl_final(data: bytes) -> bytes:
    """'R'(12) AuthenticationSASLFinal — the SCRAM server-final message."""
    return _msg("R", _INT32.pack(12) + data)


def parse_sasl_initial_response(payload: bytes) -> tuple[str, bytes]:
    """Client 'p' SASLInitialResponse: (mechanism, initial_response_bytes)."""
    mech, offset = _read_cstr(payload, 0)
    (length,) = _INT32.unpack_from(payload, offset)
    offset += 4
    data = b"" if length <= 0 else payload[offset : offset + length]
    return mech, data


def parse_sasl_response(payload: bytes) -> bytes:
    """Client 'p' SASLResponse: the raw client-final message bytes."""
    return payload


def build_sasl_initial_response(mechanism: str, data: bytes) -> bytes:
    """Client-side helper: 'p' SASLInitialResponse."""
    return _msg("p", _cstr(mechanism) + _INT32.pack(len(data)) + data)


def build_sasl_response(data: bytes) -> bytes:
    """Client-side helper: 'p' SASLResponse."""
    return _msg("p", data)


def parse_authentication(payload: bytes) -> tuple[int, bytes]:
    """Client-side helper: split an 'R' message into (subtype, data)."""
    (subtype,) = _INT32.unpack_from(payload, 0)
    return subtype, payload[4:]


def parameter_status(name: str, value: str) -> bytes:
    return _msg("S", _cstr(name) + _cstr(value))


def backend_key_data(pid: int, secret: int) -> bytes:
    return _msg("K", struct.pack("!ii", pid, secret))


def ready_for_query(status: bytes = b"I") -> bytes:
    return _msg("Z", status)


# Fixed-width type OID -> pg_type.typlen (drivers surface it as Column
# internal_size). Variable-width types report -1.
_TYPLEN: dict[int, int] = {
    16: 1,  # bool
    20: 8,  # int8
    21: 2,  # int2
    23: 4,  # int4
    26: 4,  # oid
    700: 4,  # float4
    701: 8,  # float8
    1082: 4,  # date
    1083: 8,  # time
    1114: 8,  # timestamp
    1184: 8,  # timestamptz
    1186: 16,  # interval
    1266: 12,  # timetz
    2950: 16,  # uuid
    790: 8,  # money
}


def row_description(
    columns: list[tuple[str, int] | tuple[str, int, int]],
    formats: list[int] | None = None,
    encoding: str | None = None,
) -> bytes:
    """``columns`` is a list of (name, type_oid[, typmod]); ``formats`` the
    per-column result format codes (0=text, 1=binary), defaulting to all-text.
    Column names encode in the client's ``client_encoding`` (``encoding``;
    UTF-8 when None). A missing typmod emits -1 (no modifier)."""
    payload = bytearray(_INT16.pack(len(columns)))
    for i, col in enumerate(columns):
        name, type_oid = col[0], col[1]
        typmod = col[2] if len(col) > 2 else -1
        payload += name.encode(encoding or "utf-8", errors="replace") + b"\x00"
        payload += _INT32.pack(0)  # table OID (unknown)
        payload += _INT16.pack(0)  # column attribute number
        payload += _INT32.pack(type_oid)
        payload += _INT16.pack(_TYPLEN.get(type_oid, -1))  # type size
        payload += _INT32.pack(typmod)  # type modifier
        payload += _INT16.pack(formats[i] if formats is not None else 0)  # 0=text, 1=binary
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


def notification_response(pid: int, channel: str, payload: str = "") -> bytes:
    """'A' NotificationResponse — an async LISTEN/NOTIFY delivery: the notifying
    backend's pid, the channel name, and the (possibly empty) payload."""
    return _msg("A", _INT32.pack(pid & 0x7FFFFFFF) + _cstr(channel) + _cstr(payload))


def empty_query_response() -> bytes:
    return _msg("I", b"")


def error_response(
    sqlstate: str,
    message: str,
    severity: str = "ERROR",
    encoding: str | None = "utf-8",
    *,
    diag: dict[str, str] | None = None,
    position: int | None = None,
) -> bytes:
    # ErrorResponse is a sequence of (field-type byte, value cstring), ending
    # with a single NUL. S/V severity, C SQLSTATE, M human message are the
    # minimum libpq surfaces; ``diag`` adds the optional identity fields
    # (s=schema, t=table, c=column, n=constraint, d=datatype) and ``position``
    # the 1-based statement position (clients render the LINE context from it).
    payload = bytearray(
        b"S"
        + _cstr(severity)
        + b"V"
        + _cstr(severity)
        + b"C"
        + _cstr(sqlstate)
        + b"M"
        + encode_text(message, encoding, lossy=True)
        + b"\x00"
    )
    for key, value in (diag or {}).items():
        if value:
            payload += key.encode("ascii") + encode_text(str(value), encoding, lossy=True) + b"\x00"
    if position is not None and position > 0:
        payload += b"P" + _cstr(str(position))
    payload += b"\x00"
    return _msg("E", bytes(payload))


def notice_response(
    message: str,
    severity: str = "NOTICE",
    sqlstate: str = "00000",
    encoding: str | None = "utf-8",
) -> bytes:
    payload = (
        b"S"
        + _cstr(severity)
        + b"V"
        + _cstr(severity)
        + b"C"
        + _cstr(sqlstate)
        + b"M"
        + encode_text(message, encoding, lossy=True)
        + b"\x00"
        + b"\x00"
    )
    return _msg("N", payload)


def copy_in_response(column_count: int, *, binary: bool = False) -> bytes:
    """``CopyInResponse`` ('G') — the server's go-ahead for ``COPY … FROM STDIN``.
    All columns use the overall format (0=text / 1=binary)."""
    fmt = 1 if binary else 0
    payload = bytearray([fmt]) + _INT16.pack(column_count)
    for _ in range(column_count):
        payload += _INT16.pack(fmt)
    return _msg("G", bytes(payload))


def copy_out_response(column_count: int, *, binary: bool = False) -> bytes:
    """``CopyOutResponse`` ('H') — the server starting ``COPY … TO STDOUT``."""
    fmt = 1 if binary else 0
    payload = bytearray([fmt]) + _INT16.pack(column_count)
    for _ in range(column_count):
        payload += _INT16.pack(fmt)
    return _msg("H", bytes(payload))


def copy_data(data: bytes) -> bytes:
    """``CopyData`` ('d') — one chunk of copy stream bytes (either direction)."""
    return _msg("d", data)


def copy_done() -> bytes:
    """``CopyDone`` ('c') — end of a copy stream."""
    return _msg("c", b"")


def copy_fail(message: str) -> bytes:
    """``CopyFail`` ('f') — the client aborting a ``COPY … FROM STDIN``."""
    return _msg("f", _cstr(message))


def parse_complete() -> bytes:
    return _msg("1", b"")


def bind_complete() -> bytes:
    return _msg("2", b"")


def close_complete() -> bytes:
    return _msg("3", b"")


def parameter_description(type_oids: list[int]) -> bytes:
    payload = bytearray(_INT16.pack(len(type_oids)))
    for oid in type_oids:
        payload += _INT32.pack(oid)
    return _msg("t", bytes(payload))


def no_data() -> bytes:
    return _msg("n", b"")


def portal_suspended() -> bytes:
    return _msg("s", b"")


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


def build_parse(statement: str, query: str, param_oids: list[int] | None = None) -> bytes:
    """Client-side helper: 'P' Parse."""
    oids = param_oids or []
    payload = bytearray(_cstr(statement) + _cstr(query) + _INT16.pack(len(oids)))
    for oid in oids:
        payload += _INT32.pack(oid)
    return _msg("P", bytes(payload))


def build_bind(
    portal: str,
    statement: str,
    params: list[bytes | None],
) -> bytes:
    """Client-side helper: 'B' Bind with all params in text format, text results."""
    payload = bytearray(_cstr(portal) + _cstr(statement))
    payload += _INT16.pack(0)  # zero format codes => all params text
    payload += _INT16.pack(len(params))
    for p in params:
        if p is None:
            payload += _INT32.pack(-1)
        else:
            payload += _INT32.pack(len(p)) + p
    payload += _INT16.pack(0)  # zero result format codes => all results text
    return _msg("B", bytes(payload))


def build_describe(kind: str, name: str = "") -> bytes:
    return _msg("D", kind.encode("latin-1") + _cstr(name))


def build_execute(portal: str = "", max_rows: int = 0) -> bytes:
    return _msg("E", _cstr(portal) + _INT32.pack(max_rows))


def build_close(kind: str, name: str = "") -> bytes:
    return _msg("C", kind.encode("latin-1") + _cstr(name))


def build_sync() -> bytes:
    return _msg("S", b"")


def parse_parameter_description(payload: bytes) -> list[int]:
    (count,) = _INT16.unpack_from(payload, 0)
    return [_INT32.unpack_from(payload, 2 + 4 * i)[0] for i in range(count)]


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
