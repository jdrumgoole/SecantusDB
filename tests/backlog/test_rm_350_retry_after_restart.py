"""RM-350: a retryable write retried after the server restarts.

mongod keeps what a retryable write did in `config.transactions` and the
oplog, so a retry after a restart is answered from them and applies nothing
twice. Measured on mongod 8.2.11, a one-member replica set with the
session parameters at their defaults, stopped and started again on the
same dbpath (2026-10-27; the shapes and the SIGKILL 2026-11-08); the
answers are literals here because restarting the module's reference server
is not something the fixtures do.
"""

from __future__ import annotations

import contextlib
import signal
import subprocess
import uuid
from collections.abc import Iterator
from typing import Any

import bson
import pytest
from bson import Binary, Int64

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output


@contextlib.contextmanager
def _rust_on(store: str, *flags: str, crash: bool = False) -> Iterator[tuple[str, int]]:
    """A `secantusd-rs` on `store`, which outlives it; `crash` stops it with
    SIGKILL rather than a clean shutdown."""
    assert RS_BINARY.exists(), f"{RS_BINARY} is not built"
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", store, *flags],
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
        if crash:
            proc.send_signal(signal.SIGKILL)
        else:
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


_INSERT = {"insert": "c", "documents": [{"_id": 1}, {"_id": 2}], "$db": "rm350"}
_FAM = {
    "findAndModify": "c",
    "query": {"_id": 1},
    "update": {"$inc": {"n": 1}},
    "new": True,
    "$db": "rm350",
}
_RETRIED = (
    {"n": 2, "retriedStmtIds": [0, 1], "ok": 1.0},
    {
        "lastErrorObject": {"n": 1, "updatedExisting": True},
        "value": {"_id": 1, "n": 1},
        "retriedStmtId": 0,
        "ok": 1.0,
    },
    [{"_id": 1, "n": 1}, {"_id": 2}],
)


def _session() -> dict:
    return {"id": Binary(uuid.uuid4().bytes, 4)}


@pytest.mark.backlog_fixed("RM-350")
def test_a_retry_after_a_restart_applies_nothing_twice(tmp_path: Any) -> None:
    """mongod 8.2.11 answers a retryable insert retried after a restart with
    `retriedStmtIds` and a `findAndModify` with the image it returned
    (`retriedStmtId: 0`), applying neither again. This server forgot what
    its retryable writes did when it stopped: the insert failed on a
    duplicate key and the `findAndModify` ran a second time."""
    store = str(tmp_path / "store")
    a, b = _session(), _session()
    with _rust_on(store) as address:
        _op_msg(address, {**_INSERT, "lsid": a, "txnNumber": Int64(1)})
        _op_msg(address, {**_FAM, "lsid": b, "txnNumber": Int64(1)})
    with _rust_on(store) as address:
        retried_insert = _op_msg(address, {**_INSERT, "lsid": a, "txnNumber": Int64(1)})
        retried_fam = _op_msg(address, {**_FAM, "lsid": b, "txnNumber": Int64(1)})
        docs = _op_msg(address, {"find": "c", "sort": {"_id": 1}, "$db": "rm350"})["cursor"][
            "firstBatch"
        ]
    assert (retried_insert, retried_fam, docs) == _RETRIED


@pytest.mark.backlog_fixed("RM-350")
def test_each_kind_of_retry_after_a_restart(tmp_path: Any) -> None:
    """mongod 8.2.11 answers every kind of retryable write retried after a
    restart from what it kept: an update, an upsert, a delete, a removing
    and an upserting `findAndModify`, a batch whose first statement ran (the
    second runs now), and a number older than the session's (225). This
    server ran each of them again."""
    store = str(tmp_path / "store")
    db = "rm350s"
    first = [
        {"update": "c", "updates": [{"q": {"_id": 3}, "u": {"$inc": {"n": 1}}}]},
        {"update": "c", "updates": [{"q": {"_id": 9}, "u": {"$set": {"x": 1}}, "upsert": True}]},
        {"delete": "c", "deletes": [{"q": {"_id": 4}, "limit": 1}]},
        {"findAndModify": "c", "query": {"_id": 5}, "remove": True},
        {"insert": "c", "documents": [{"_id": 10}]},
        {
            "findAndModify": "c",
            "query": {"_id": 20},
            "update": {"$set": {"z": 1}},
            "upsert": True,
            "new": True,
        },
    ]
    retried = [*first[:4], {"insert": "c", "documents": [{"_id": 10}, {"_id": 11}]}, first[5]]
    sessions = [_session() for _ in first]
    seed = {"insert": "c", "documents": [{"_id": 3, "n": 0}, {"_id": 4}, {"_id": 5}], "$db": db}
    with _rust_on(store) as address:
        _op_msg(address, seed)
        for lsid, cmd in zip(sessions, first, strict=True):
            _op_msg(address, {**cmd, "$db": db, "lsid": lsid, "txnNumber": Int64(1)})
    with _rust_on(store) as address:
        answers = [
            _op_msg(address, {**cmd, "$db": db, "lsid": lsid, "txnNumber": Int64(1)})
            for lsid, cmd in zip(sessions, retried, strict=True)
        ]
        too_old = _op_msg(
            address, {**first[0], "$db": db, "lsid": sessions[0], "txnNumber": Int64(0)}
        )
        docs = _op_msg(address, {"find": "c", "sort": {"_id": 1}, "$db": db})["cursor"][
            "firstBatch"
        ]
    assert answers == [
        {"n": 1, "retriedStmtIds": [0], "nModified": 1, "ok": 1.0},
        {
            "n": 1,
            "retriedStmtIds": [0],
            "upserted": [{"index": 0, "_id": 9}],
            "nModified": 0,
            "ok": 1.0,
        },
        {"n": 1, "retriedStmtIds": [0], "ok": 1.0},
        {"lastErrorObject": {"n": 1}, "value": {"_id": 5}, "retriedStmtId": 0, "ok": 1.0},
        {"n": 2, "retriedStmtIds": [0], "ok": 1.0},
        {
            "lastErrorObject": {"n": 1, "updatedExisting": False, "upserted": 20},
            "value": {"_id": 20, "z": 1},
            "retriedStmtId": 0,
            "ok": 1.0,
        },
    ]
    assert (too_old["code"], too_old["errmsg"].split(" on session ")[0]) == (
        225,
        "Retryable write with txnNumber 0 is prohibited",
    )
    assert docs == [
        {"_id": 3, "n": 1},
        {"_id": 9, "x": 1},
        {"_id": 10},
        {"_id": 11},
        {"_id": 20, "z": 1},
    ]


@pytest.mark.backlog_fixed("RM-350")
def test_a_retry_after_a_crash_under_data_nonlogged_applies_nothing_twice(tmp_path: Any) -> None:
    """mongod 8.2.11 killed with SIGKILL after two journaled retryable
    writes answers their retries as after a clean restart: recovery rebuilds
    the session's `config.transactions` row and the `findAndModify`'s image
    from the oplog. This server under `--data-nonlogged` wrote both without
    an oplog entry and recovered its data tables by replaying the oplog, so a
    crash lost them: the insert failed on a duplicate key and the
    `findAndModify` ran a second time. Replay now rebuilds both, as mongod's
    `SessionUpdateTracker` does."""
    store = str(tmp_path / "store")
    a, b = _session(), _session()
    journaled = {"writeConcern": {"j": True}}
    with _rust_on(store, "--data-nonlogged", crash=True) as address:
        _op_msg(address, {**_INSERT, "lsid": a, "txnNumber": Int64(1), **journaled})
        _op_msg(address, {**_FAM, "lsid": b, "txnNumber": Int64(1), **journaled})
    with _rust_on(store, "--data-nonlogged") as address:
        retried_insert = _op_msg(address, {**_INSERT, "lsid": a, "txnNumber": Int64(1)})
        retried_fam = _op_msg(address, {**_FAM, "lsid": b, "txnNumber": Int64(1)})
        docs = _op_msg(address, {"find": "c", "sort": {"_id": 1}, "$db": "rm350"})["cursor"][
            "firstBatch"
        ]
    assert (retried_insert, retried_fam, docs) == _RETRIED
