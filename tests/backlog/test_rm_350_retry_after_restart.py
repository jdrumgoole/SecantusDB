"""RM-350: a retryable write retried after the server restarts.

mongod keeps what a retryable write did in `config.transactions` and the
oplog, so a retry after a restart is answered from them and applies nothing
twice. Measured on mongod 8.2.11, a one-member replica set with the
session parameters at their defaults, stopped and started again on the
same dbpath (2026-10-27); the answers are literals here because restarting
the module's reference server is not something the fixtures do.
"""

from __future__ import annotations

import contextlib
import subprocess
import uuid
from collections.abc import Iterator
from typing import Any

import bson
import pytest
from bson import Binary, Int64

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output


@contextlib.contextmanager
def _rust_on(store: str) -> Iterator[tuple[str, int]]:
    """A `secantusd-rs` on `store`, which outlives it."""
    assert RS_BINARY.exists(), f"{RS_BINARY} is not built"
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", store],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert proc.stdout is not None
        line, m = "", None
        for _ in range(20):
            line = proc.stdout.readline()
            m = _RS_BANNER.search(line)
            if m or not line:
                break
        assert m, f"secantusd-rs did not start: {line!r}"
        drain_output(proc)
        yield m.group(1), int(m.group(2))
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()


def _op_msg(address: tuple[str, int], command: dict) -> dict:
    """One command as a raw OP_MSG: a driver would replace the `lsid`."""
    import socket
    import struct

    body = b"\x00" + bson.encode(command)
    msg = struct.pack("<iiii", 16 + 4 + len(body), 1, 0, 2013) + struct.pack("<I", 0) + body
    with socket.create_connection(address, timeout=60) as sock:
        sock.sendall(msg)
        head = b""
        while len(head) < 4:
            head += sock.recv(4 - len(head))
        (length,) = struct.unpack("<i", head)
        rest = b""
        while len(rest) < length - 4:
            rest += sock.recv(length - 4 - len(rest))
    reply = bson.decode(rest[12 + 4 + 1 :])
    for k in ("$clusterTime", "operationTime", "electionId", "opTime"):
        reply.pop(k, None)
    return reply


@pytest.mark.backlog("RM-350")
def test_a_retry_after_a_restart_applies_nothing_twice(tmp_path: Any) -> None:
    """mongod 8.2.11 answers a retryable insert retried after a restart with
    `retriedStmtIds` and a `findAndModify` with the image it returned
    (`retriedStmtId: 0`), applying neither again. This server forgets what
    its retryable writes did when it stops: the insert fails on a duplicate
    key and the `findAndModify` runs a second time."""
    store = str(tmp_path / "store")
    insert = {"insert": "c", "documents": [{"_id": 1}, {"_id": 2}], "$db": "rm350"}
    fam = {
        "findAndModify": "c",
        "query": {"_id": 1},
        "update": {"$inc": {"n": 1}},
        "new": True,
        "$db": "rm350",
    }
    a = {"id": Binary(uuid.uuid4().bytes, 4)}
    b = {"id": Binary(uuid.uuid4().bytes, 4)}
    with _rust_on(store) as address:
        _op_msg(address, {**insert, "lsid": a, "txnNumber": Int64(1)})
        _op_msg(address, {**fam, "lsid": b, "txnNumber": Int64(1)})
    with _rust_on(store) as address:
        retried_insert = _op_msg(address, {**insert, "lsid": a, "txnNumber": Int64(1)})
        retried_fam = _op_msg(address, {**fam, "lsid": b, "txnNumber": Int64(1)})
        docs = _op_msg(address, {"find": "c", "$db": "rm350"})["cursor"]["firstBatch"]
    assert (retried_insert, retried_fam, docs) == (
        {"n": 2, "retriedStmtIds": [0, 1], "ok": 1.0},
        {
            "lastErrorObject": {"n": 1, "updatedExisting": True},
            "value": {"_id": 1, "n": 1},
            "retriedStmtId": 0,
            "ok": 1.0,
        },
        [{"_id": 1, "n": 1}, {"_id": 2}],
    )
