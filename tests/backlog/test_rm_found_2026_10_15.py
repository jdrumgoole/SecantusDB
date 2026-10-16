"""Gap tests for Rust MongoDB server items found during Phase 2 batch 2 on
2026-10-15 (measured against mongod 8.2.11).

The first three are wire-level and use a raw socket: a driver never sends
an unknown opcode or a legacy OP_GET_MORE, and it hides how an exhaust
stream is framed.
"""

from __future__ import annotations

import socket
import struct
from typing import Any

import bson
import pymongo
import pytest
from bson.int64 import Int64

from tests.backlog.conftest import mongo_answer

OP_REPLY = 1
OP_GET_MORE = 2005
OP_MSG = 2013
MORE_TO_COME = 1 << 1
EXHAUST_ALLOWED = 1 << 16


def _connect(client: Any) -> socket.socket:
    client.admin.command("ping")  # so `address` is known
    host, port = client.address
    return socket.create_connection((host, port), timeout=5)


def _read(sock: socket.socket, n: int) -> bytes:
    out = b""
    while len(out) < n:
        chunk = sock.recv(n - len(out))
        if not chunk:
            raise ConnectionError("closed")
        out += chunk
    return out


def _frame(sock: socket.socket) -> tuple[int, int, bytes]:
    """`(opcode, responseTo, body)` of the next frame."""
    length, _, response_to, opcode = struct.unpack("<iiii", _read(sock, 16))
    return opcode, response_to, _read(sock, length - 16)


def _send_msg(sock: socket.socket, request_id: int, command: dict, flags: int = 0) -> None:
    payload = struct.pack("<I", flags) + b"\x00" + bson.encode(command)
    sock.sendall(struct.pack("<iiii", 16 + len(payload), request_id, 0, OP_MSG) + payload)


def _ping_after(sock: socket.socket, request_id: int) -> tuple:
    """Send a ping and return `("ok", opcode, responseTo, the reply's ok)`,
    or `("closed", error)` when the server has dropped the connection."""
    try:
        _send_msg(sock, request_id, {"ping": 1, "$db": "admin"})
        opcode, response_to, body = _frame(sock)
    except OSError as e:
        return ("closed", type(e).__name__)
    return ("ok", opcode, response_to, bson.decode(body[5:])["ok"])


@pytest.mark.backlog_fixed("RM-144")
@pytest.mark.parametrize("opcode", [9999, OP_REPLY])
def test_unknown_opcode_is_ignored_and_the_connection_kept(rust_mongo: Any, opcode: int) -> None:
    """mongod 8.2.11 ignores a frame with an opcode it does not serve (9999,
    or OP_REPLY sent as a request): no reply, and the connection stays open,
    so a ping sent next on the same socket is answered `{ok: 1.0}`. The Rust
    server reset the connection."""
    sock = _connect(rust_mongo)
    try:
        junk = b"\x00" * 20
        sock.sendall(struct.pack("<iiii", 16 + len(junk), 7, 0, opcode) + junk)
        got = _ping_after(sock, 8)
    finally:
        sock.close()
    assert got == ("ok", OP_MSG, 8, 1.0)


@pytest.mark.backlog_fixed("RM-145")
def test_legacy_op_get_more_is_refused_with_an_op_reply(rust_mongo: Any) -> None:
    """mongod 8.2.11 answers a legacy OP_GET_MORE with an OP_REPLY:
    responseFlags 2 (QueryFailure), cursorId 0, startingFrom 0, one document
    `{$err: "OP_GET_MORE is no longer supported. ...", code: 5739101, ok: 0.0}`,
    and keeps the connection (a ping next is answered). The Rust server closed
    the connection."""
    sock = _connect(rust_mongo)
    try:
        body = struct.pack("<i", 0) + b"gaptests_x.c\x00" + struct.pack("<iq", 0, 12345)
        sock.sendall(struct.pack("<iiii", 16 + len(body), 11, 0, OP_GET_MORE) + body)
        try:
            opcode, response_to, reply = _frame(sock)
            flags, cursor_id, starting_from, returned = struct.unpack("<iqii", reply[:20])
            got = (
                opcode,
                response_to,
                flags,
                cursor_id,
                starting_from,
                returned,
                list(bson.decode(reply[20:]).items()),
            )
        except OSError as e:
            got = ("closed", type(e).__name__)
        after = _ping_after(sock, 12)
    finally:
        sock.close()
    assert got == (
        OP_REPLY,
        11,
        2,
        0,
        0,
        1,
        [
            (
                "$err",
                "OP_GET_MORE is no longer supported. The client driver may require an "
                "upgrade. For more details see "
                "https://dochub.mongodb.org/core/legacy-opcode-removal",
            ),
            ("code", 5739101),
            ("ok", 0.0),
        ],
    )
    assert after[0] == "ok"


@pytest.mark.backlog_fixed("RM-146")
def test_exhaust_get_more_ends_with_the_short_batch(rust_mongo: Any) -> None:
    """mongod 8.2.11, 10 documents, `find` batchSize 3, then a getMore with
    `exhaustAllowed`: three frames -- [3,4,5] and [6,7,8] with moreToCome and
    the live cursor id, then [9] WITHOUT moreToCome and cursor id 0. The
    short batch ends the stream. The Rust server sent [9] with moreToCome and
    the live id, then a fourth, empty frame with cursor id 0."""
    coll = rust_mongo["gaptests_exhaust"]["c"]
    coll.drop()
    coll.insert_many([{"_id": i} for i in range(10)])
    sock = _connect(rust_mongo)
    frames: list[tuple] = []
    try:
        _send_msg(
            sock, 21, {"find": "c", "batchSize": 3, "sort": {"_id": 1}, "$db": "gaptests_exhaust"}
        )
        cursor = bson.decode(_frame(sock)[2][5:])["cursor"]
        assert [d["_id"] for d in cursor["firstBatch"]] == [0, 1, 2]
        _send_msg(
            sock,
            22,
            {"getMore": cursor["id"], "collection": "c", "batchSize": 3, "$db": "gaptests_exhaust"},
            flags=EXHAUST_ALLOWED,
        )
        while len(frames) < 6:
            try:
                body = _frame(sock)[2]
            except OSError as e:
                frames.append(("closed", type(e).__name__))
                break
            flags = struct.unpack("<I", body[:4])[0]
            cur = bson.decode(body[5:])["cursor"]
            frames.append(
                (
                    bool(flags & MORE_TO_COME),
                    "live" if cur["id"] else 0,
                    [d["_id"] for d in cur["nextBatch"]],
                )
            )
            if not flags & MORE_TO_COME:
                break
    finally:
        sock.close()
        coll.drop()
    assert frames == [
        (True, "live", [3, 4, 5]),
        (True, "live", [6, 7, 8]),
        (False, 0, [9]),
    ]


@pytest.mark.backlog_fixed("RM-147")
def test_server_status_reports_compression_counters(rust_mongo: Any) -> None:
    """mongod 8.2.11 reports `network.compression` with all three compressors,
    each `{compressor: {bytesIn, bytesOut}, decompressor: {bytesIn, bytesOut}}`
    as int64 -- present at zero on a fresh server -- and after zlib-compressed
    traffic the zlib counters are above zero (e.g. compressor 294/186,
    decompressor 505/674 after two pings). The Rust server's `network` has
    only bytesIn / bytesOut / numRequests."""
    rust_mongo.admin.command("ping")  # so `address` is known
    host, port = rust_mongo.address
    zlib_client = pymongo.MongoClient(
        host=host, port=port, compressors="zlib", serverSelectionTimeoutMS=10_000
    )
    try:
        zlib_client.admin.command("ping")
        zlib_client.admin.command("ping")
        status = zlib_client.admin.command("serverStatus")
    finally:
        zlib_client.close()
    compression = status["network"].get("compression")
    shape = (
        None
        if compression is None
        else {
            name: {side: sorted(counts) for side, counts in sides.items()}
            for name, sides in compression.items()
        }
    )
    counts = {"compressor": ["bytesIn", "bytesOut"], "decompressor": ["bytesIn", "bytesOut"]}
    assert shape == {"snappy": counts, "zstd": counts, "zlib": counts}
    assert all(
        isinstance(n, Int64)
        for sides in compression.values()
        for side in sides.values()
        for n in side.values()
    )
    assert compression["zlib"]["decompressor"]["bytesIn"] > 0
    assert compression["zlib"]["compressor"]["bytesOut"] > 0


@pytest.mark.backlog("RM-035")
def test_scram_iteration_count_is_settable_at_runtime(rust_mongo: Any) -> None:
    """mongod 8.2.11 reports `scramIterationCount` as settableAtRuntime and
    sets it: `setParameter {scramIterationCount: 12000}` answers
    `{was: 10000, ok: 1}` and `getParameter` reads back 12000. The Rust
    server reports the parameter but refuses to change it (20 "not allowed to
    change [scramIterationCount] at runtime")."""
    admin = rust_mongo.admin
    details = mongo_answer(
        lambda: admin.command({"getParameter": {"showDetails": True}, "scramIterationCount": 1})[
            "scramIterationCount"
        ]
    )
    got = mongo_answer(lambda: admin.command("setParameter", 1, scramIterationCount=12000)["was"])
    back = mongo_answer(
        lambda: admin.command("getParameter", 1, scramIterationCount=1)["scramIterationCount"]
    )
    mongo_answer(lambda: admin.command("setParameter", 1, scramIterationCount=10000))
    assert (got, back, details) == (
        ("ok", 10000),
        ("ok", 12000),
        ("ok", {"value": 10000, "settableAtRuntime": True, "settableAtStartup": True}),
    )
