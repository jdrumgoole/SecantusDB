from __future__ import annotations

import socket
import struct
from dataclasses import dataclass, field
from typing import Any

import bson

OP_REPLY = 1
OP_QUERY = 2004
OP_GET_MORE = 2005
OP_MSG = 2013
OP_COMPRESSED = 2012

# OP_MSG flag bits.
OP_MSG_FLAG_CHECKSUM_PRESENT = 1 << 0
OP_MSG_FLAG_MORE_TO_COME = 1 << 1
OP_MSG_FLAG_EXHAUST_ALLOWED = 1 << 16

_HEADER = struct.Struct("<iiii")
_INT32 = struct.Struct("<i")
_UINT32 = struct.Struct("<I")
_UINT8 = struct.Struct("<B")

HEADER_SIZE = _HEADER.size

MAX_MESSAGE_SIZE = 48_000_000
MAX_BSON_OBJECT_SIZE = 16 * 1024 * 1024


class WireProtocolError(Exception):
    pass


class MalformedBodyError(WireProtocolError):
    """Body bytes parse cleanly as a wire frame but fail BSON validation.

    Raised after the message header has been read — the caller (the
    server's connection loop) uses ``header.request_id`` to build a
    targeted ``{ok: 0, errmsg, code: 2 BadValue}`` reply so the
    connection survives. mongod returns the same shape for
    e.g. ``BinData(4, ...)`` payloads whose advertised binary
    length doesn't match the actual byte count.
    """

    def __init__(self, header: Header, message: str) -> None:
        super().__init__(message)
        self.header = header


class _BodyBoundsError(Exception):
    """Internal: a BSON length field in an OP_MSG body is out-of-range.

    Raised by the pre-decode bounds checks in ``_parse_op_msg``. Caught
    by ``read_message`` and re-raised as ``MalformedBodyError`` so the
    connection loop treats it as a recoverable BadValue (same shape as
    a downstream ``bson.InvalidBSON``) rather than a fatal protocol
    error that drops the connection.
    """


class ConnectionClosed(Exception):
    pass


@dataclass
class Header:
    message_length: int
    request_id: int
    response_to: int
    op_code: int

    def pack(self) -> bytes:
        return _HEADER.pack(self.message_length, self.request_id, self.response_to, self.op_code)

    @classmethod
    def unpack(cls, buf: bytes) -> Header:
        return cls(*_HEADER.unpack(buf))


@dataclass
class OpMsg:
    flags: int = 0
    body: dict[str, Any] = field(default_factory=dict)
    document_sequences: list[tuple[str, list[dict[str, Any]]]] = field(default_factory=list)


@dataclass
class OpQuery:
    flags: int
    full_collection_name: str
    number_to_skip: int
    number_to_return: int
    query: dict[str, Any]
    return_fields_selector: dict[str, Any] | None = None


@dataclass
class Message:
    header: Header
    op: OpMsg | OpQuery


def _recv_exactly(sock: socket.socket, n: int) -> bytes:
    chunks: list[bytes] = []
    remaining = n
    while remaining > 0:
        chunk = sock.recv(remaining)
        if not chunk:
            raise ConnectionClosed
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_message(sock: socket.socket) -> Message:
    header_bytes = _recv_exactly(sock, HEADER_SIZE)
    header = Header.unpack(header_bytes)
    if header.message_length < HEADER_SIZE:
        raise WireProtocolError(f"message length {header.message_length} < header size")
    if header.message_length > MAX_MESSAGE_SIZE:
        raise WireProtocolError(
            f"message length {header.message_length} exceeds max {MAX_MESSAGE_SIZE}"
        )
    body = _recv_exactly(sock, header.message_length - HEADER_SIZE)
    try:
        if header.op_code == OP_MSG:
            return Message(header=header, op=_parse_op_msg(body))
        if header.op_code == OP_QUERY:
            return Message(header=header, op=_parse_op_query(body))
    except (bson.InvalidBSON, _BodyBoundsError) as exc:
        # Client sent a body whose framing is plausible but whose
        # inner BSON document fails validation (e.g. a BinData with
        # a mismatched length field, or an int32 declaring a doc size
        # that overflows the buffer). Real mongod replies with a
        # ``BadValue`` error and keeps the connection alive — surface
        # the header here so the connection loop can do the same
        # rather than dropping the socket on the next message.
        raise MalformedBodyError(header, f"invalid BSON in body: {exc}") from exc
    raise WireProtocolError(f"unsupported op_code {header.op_code}")


def _parse_op_query(buf: bytes) -> OpQuery:
    if len(buf) < 4:
        raise WireProtocolError("OP_QUERY body too short")
    (flags,) = _UINT32.unpack_from(buf, 0)
    offset = 4
    name_end = buf.index(b"\x00", offset)
    full_collection_name = buf[offset:name_end].decode("utf-8")
    offset = name_end + 1
    (number_to_skip,) = _INT32.unpack_from(buf, offset)
    offset += 4
    (number_to_return,) = _INT32.unpack_from(buf, offset)
    offset += 4
    (doc_len,) = _INT32.unpack_from(buf, offset)
    query = bson.decode(buf[offset : offset + doc_len])
    offset += doc_len
    selector: dict[str, Any] | None = None
    if offset < len(buf):
        (sel_len,) = _INT32.unpack_from(buf, offset)
        selector = bson.decode(buf[offset : offset + sel_len])
    return OpQuery(
        flags=flags,
        full_collection_name=full_collection_name,
        number_to_skip=number_to_skip,
        number_to_return=number_to_return,
        query=query,
        return_fields_selector=selector,
    )


# Minimum BSON document length (an empty doc is 5 bytes: 4-byte length
# + 1-byte zero terminator). Anything smaller is malformed.
_MIN_BSON_DOC_LEN = 5


def _check_doc_len(doc_len: int, offset: int, end: int, where: str) -> None:
    """Validate a BSON length field before slicing into ``buf``.

    Without this check, an attacker-supplied negative `doc_len` produces
    an empty-or-garbage slice that `bson.decode` raises on uncaught,
    crashing the connection thread. An oversized `doc_len` either
    short-reads (decode fails partway) or attempts to allocate the
    declared size. Raises ``_BodyBoundsError`` (translated to
    ``MalformedBodyError`` by ``read_message``) so the connection loop
    treats it as a recoverable BadValue, matching mongod.
    """
    if doc_len < _MIN_BSON_DOC_LEN:
        raise _BodyBoundsError(
            f"{where}: declared BSON length {doc_len} below minimum {_MIN_BSON_DOC_LEN}"
        )
    if offset + doc_len > end:
        raise _BodyBoundsError(
            f"{where}: declared BSON length {doc_len} would read "
            f"past message end (offset={offset}, end={end})"
        )


def _parse_op_msg(buf: bytes) -> OpMsg:
    if len(buf) < 4:
        raise WireProtocolError("OP_MSG body too short for flags")
    (flags,) = _UINT32.unpack_from(buf, 0)
    has_checksum = bool(flags & (1 << 0))
    end = len(buf) - 4 if has_checksum else len(buf)

    offset = 4
    body: dict[str, Any] | None = None
    sequences: list[tuple[str, list[dict[str, Any]]]] = []

    while offset < end:
        (kind,) = _UINT8.unpack_from(buf, offset)
        offset += 1
        if kind == 0:
            if offset + 4 > end:
                raise _BodyBoundsError("OP_MSG kind-0 truncated before length")
            (doc_len,) = _INT32.unpack_from(buf, offset)
            _check_doc_len(doc_len, offset, end, "OP_MSG kind-0")
            doc_bytes = buf[offset : offset + doc_len]
            if body is not None:
                raise _BodyBoundsError("OP_MSG has more than one kind-0 section")
            body = bson.decode(doc_bytes)
            offset += doc_len
        elif kind == 1:
            if offset + 4 > end:
                raise _BodyBoundsError("OP_MSG kind-1 truncated before length")
            (section_len,) = _INT32.unpack_from(buf, offset)
            # section_len includes the 4 length bytes themselves.
            if section_len < 4 or offset + section_len > end:
                raise _BodyBoundsError(
                    f"OP_MSG kind-1: declared section length {section_len} invalid"
                )
            section_end = offset + section_len
            offset += 4
            ident_end = buf.index(b"\x00", offset, section_end)
            identifier = buf[offset:ident_end].decode("utf-8")
            offset = ident_end + 1
            docs: list[dict[str, Any]] = []
            while offset < section_end:
                if offset + 4 > section_end:
                    raise _BodyBoundsError("OP_MSG kind-1: truncated inner doc length")
                (doc_len,) = _INT32.unpack_from(buf, offset)
                _check_doc_len(doc_len, offset, section_end, "OP_MSG kind-1 inner")
                docs.append(bson.decode(buf[offset : offset + doc_len]))
                offset += doc_len
            sequences.append((identifier, docs))
        else:
            raise WireProtocolError(f"unknown OP_MSG section kind {kind}")

    if body is None:
        raise WireProtocolError("OP_MSG has no kind-0 body section")
    return OpMsg(flags=flags, body=body, document_sequences=sequences)


def build_op_msg_reply(
    response_to: int,
    request_id: int,
    body: dict[str, Any],
    flags: int = 0,
) -> bytes:
    body_bytes = bson.encode(body)
    payload = _UINT32.pack(flags) + b"\x00" + body_bytes
    message_length = HEADER_SIZE + len(payload)
    header = Header(
        message_length=message_length,
        request_id=request_id,
        response_to=response_to,
        op_code=OP_MSG,
    )
    return header.pack() + payload


def build_op_reply(
    response_to: int,
    request_id: int,
    documents: list[dict[str, Any]],
    cursor_id: int = 0,
    starting_from: int = 0,
    response_flags: int = 0,
) -> bytes:
    docs_bytes = b"".join(bson.encode(d) for d in documents)
    payload = (
        _UINT32.pack(response_flags)
        + struct.pack("<q", cursor_id)
        + _INT32.pack(starting_from)
        + _INT32.pack(len(documents))
        + docs_bytes
    )
    message_length = HEADER_SIZE + len(payload)
    header = Header(
        message_length=message_length,
        request_id=request_id,
        response_to=response_to,
        op_code=OP_REPLY,
    )
    return header.pack() + payload
