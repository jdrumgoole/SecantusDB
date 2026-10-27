"""Gap tests for what remains of RM-222 (strings whose bytes are not UTF-8),
and an item found while working it.

The Rust server keeps, compares, indexes and serves such strings since
2026-10-27 (`tests/mongod_live/test_invalid_utf8.py` pins that against
mongod). What is left is asked of a live mongod 8.2.11 here, over raw
OP_MSG, because pymongo will neither send nor decode these bytes.
"""

from __future__ import annotations

import socket
import struct
from typing import Any

import bson
import pytest
from bson.codec_options import CodecOptions

_SURROGATE = CodecOptions(unicode_decode_error_handler="surrogateescape")


def _cstr(b: bytes) -> bytes:
    return b + b"\x00"


def _string(b: bytes) -> bytes:
    return struct.pack("<i", len(b) + 1) + b + b"\x00"


def _doc(elements: bytes) -> bytes:
    return struct.pack("<i", 4 + len(elements) + 1) + elements + b"\x00"


def _command(client: Any, body: bytes) -> bytes:
    """Send one OP_MSG whose body is `body`; the reply body's raw bytes."""
    payload = struct.pack("<I", 0) + b"\x00" + body
    with socket.create_connection(client.address, timeout=10) as s:
        s.sendall(struct.pack("<iiii", 16 + len(payload), 1, 0, 2013) + payload)
        head = b""
        while len(head) < 16:
            chunk = s.recv(16 - len(head))
            assert chunk, "the server closed the connection"
            head += chunk
        length = struct.unpack("<i", head[:4])[0]
        data = b""
        while len(data) < length - 16:
            data += s.recv(length - 16 - len(data))
    return data[5:]


def _reply(client: Any, body: bytes) -> dict:
    return bson.decode(_command(client, body), codec_options=_SURROGATE)


# ------------------------------------------------- names that are not UTF-8 --


def _create_named(client: Any) -> tuple:
    db = b"rm222names"
    _reply(client, _doc(b"\x02drop\x00" + _string(b"c\xff") + b"\x02$db\x00" + _string(db)))
    r = _reply(client, _doc(b"\x02create\x00" + _string(b"c\xff") + b"\x02$db\x00" + _string(db)))
    return (r.get("ok"), r.get("code"))


@pytest.mark.backlog("RM-222")
def test_collection_name_that_is_not_utf8(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 creates a collection whose name holds the byte 0xff
    (ok 1); the Rust server refuses it (73 "Collection name has invalid type
    string")."""
    assert _create_named(rust_mongo) == _create_named(mongod)


# ------------------------------------- a message that quotes such a string --


def _to_int_message(client: Any) -> bytes:
    db = b"rm222msg"
    _reply(client, _doc(b"\x02drop\x00" + _string(b"c") + b"\x02$db\x00" + _string(db)))
    stage = _doc(
        b"\x03$project\x00" + _doc(b"\x03r\x00" + _doc(b"\x02$toInt\x00" + _string(b"\xff\xfe")))
    )
    pipeline = _doc(b"\x030\x00" + stage)
    body = _doc(
        b"\x02aggregate\x00"
        + _string(b"c")
        + b"\x04pipeline\x00"
        + pipeline
        + b"\x03cursor\x00"
        + _doc(b"")
        + b"\x02$db\x00"
        + _string(db)
    )
    reply = bson.decode(_command(client, body), codec_options=_SURROGATE)
    return str(reply.get("errmsg", "")).encode("utf-8", "surrogateescape")


@pytest.mark.backlog("RM-222")
def test_error_message_quotes_the_bytes(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 quotes the string's own bytes in the message
    (`Failed to parse number '\\xff\\xfe' in $convert ...`); the Rust server
    quotes it with U+FFFD for each invalid sequence."""
    assert _to_int_message(rust_mongo) == _to_int_message(mongod)


# --------------------------------- the other types that carry text --


_OTHER_TEXT = {
    "javascript": b"\x0dv\x00" + _string(b"\xff"),
    "symbol": b"\x0ev\x00" + _string(b"\xff"),
    "regex pattern": b"\x0bv\x00\xff\x00\x00",
    "dbPointer": b"\x0cv\x00" + _string(b"d.\xff") + b"\x00" * 12,
    "javascriptWithScope": b"\x0fv\x00"
    + struct.pack("<i", 4 + 6 + 5)
    + _string(b"\xff")
    + _doc(b""),
}


def _insert_other(client: Any, element: bytes) -> tuple:
    db = b"rm222other"
    _reply(client, _doc(b"\x02drop\x00" + _string(b"t") + b"\x02$db\x00" + _string(db)))
    document = _doc(b"\x10_id\x00" + struct.pack("<i", 1) + element)
    body = _doc(
        b"\x02insert\x00"
        + _string(b"t")
        + b"\x04documents\x00"
        + _doc(b"\x030\x00" + document)
        + b"\x02$db\x00"
        + _string(db)
    )
    r = _reply(client, body)
    return (r.get("ok"), r.get("n"), r.get("code"))


@pytest.mark.backlog("RM-222")
@pytest.mark.parametrize("element", list(_OTHER_TEXT.values()), ids=list(_OTHER_TEXT))
def test_other_text_types_that_are_not_utf8(rust_mongo: Any, mongod: Any, element: bytes) -> None:
    """mongod 8.2.11 stores JavaScript, a symbol, a regex, a DBPointer and
    JavaScript with a scope whose text is not UTF-8 (n 1); the Rust server
    refuses the document (2 BadValue)."""
    assert _insert_other(rust_mongo, element) == _insert_other(mongod, element)


# --------------------------------------------- found while working RM-222 --


def _group_sort(client: Any) -> list:
    coll = client.rm329.c
    coll.drop()
    coll.insert_many(
        [
            {"_id": 1, "s": ["x", "q"]},
            {"_id": 2, "s": "b"},
            {"_id": 3, "s": {"x": 1}},
            {"_id": 4, "s": "z"},
        ]
    )
    pipeline = [{"$group": {"_id": "$s", "n": {"$sum": 1}}}, {"$sort": {"_id": 1}}]
    return [d["_id"] for d in coll.aggregate(pipeline)]


@pytest.mark.backlog("RM-329")
def test_sort_of_group_ids_that_are_arrays(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sorts a `$group` key that is an array as an array,
    after the documents (`b, z, {x: 1}, [x, q]`); the Rust server sorts it by
    its smallest element, as a find sort does (`b, [x, q], z, {x: 1}`)."""
    assert _group_sort(rust_mongo) == _group_sort(mongod)
