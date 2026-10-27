"""Interrupted operations against a one-member mongod 8.2 replica set
(RM-313).

An operation is interrupted by `killOp`, by a kill of the session it runs
on (`killSessions`, `killAllSessions`, `killAllSessionsByPattern`) or by
its spent `maxTimeMS`. mongod checks for one at its interrupt points --
each document a scan reads, each element an expression like `$reduce`
walks, each statement of a write batch -- and answers 11601 (50 for a
budget), under the wrapper its command takes; a write that waits for its
write concern carries a `writeConcernError` too. What it wrote before the
interrupt stays written, unless the write is a transaction's or a
retryable insert's, which it rolls back.

Each case runs an operation made slow by a filter that folds a long
`$reduce` over the document's own fields, sized per server so one document
takes about two seconds (`Slow`), sends the kill from another connection
while it runs, and compares the replies and what the collection holds
afterwards. Raw OP_MSG on connections of their own: a driver would swap a
hand-set `lsid` for a pooled session.
"""

from __future__ import annotations

import hashlib
import socket
import struct
import threading
import time
import uuid
from typing import Any

import bson
from bson import Binary, Int64

from tests.mongod_live.conftest import same

#: Reply fields that are per-server by nature.
_PER_SERVER = {"operationTime", "$clusterTime", "electionId", "opTime"}

DB = "live_interrupts"

#: A filter that takes as long as the document's `outer` x `inner` say.
SLOW_EXPR = {
    "$gte": [
        {
            "$reduce": {
                "input": {"$range": [0, "$outer"]},
                "initialValue": 0,
                "in": {"$add": ["$$value", {"$size": {"$range": [0, "$inner"]}}]},
            }
        },
        0,
    ]
}
SLOW = {"$expr": SLOW_EXPR}


def op_msg(address: tuple, command: dict) -> dict:
    """One command as a raw OP_MSG on a connection of its own."""
    body = b"\x00" + bson.encode(command)
    msg = struct.pack("<iiii", 16 + 4 + len(body), 1, 0, 2013) + struct.pack("<I", 0) + body
    with socket.create_connection(address, timeout=120) as sock:
        sock.sendall(msg)
        head = b""
        while len(head) < 4:
            head += sock.recv(4 - len(head))
        (length,) = struct.unpack("<i", head)
        rest = b""
        while len(rest) < length - 4:
            rest += sock.recv(length - 4 - len(rest))
    return bson.decode(rest[12 + 4 + 1 :])


def clean(reply: dict) -> dict:
    return {k: v for k, v in reply.items() if k not in _PER_SERVER}


class Slow:
    """Slow documents on one server: `outer` is sized so that one document
    with `inner: 1000` takes about two seconds to test."""

    _sized: dict[tuple, int] = {}

    def __init__(self, client: Any) -> None:
        self.address = client.address
        if self.address not in self._sized:
            self._sized[self.address] = self._size()
        self.outer = self._sized[self.address]

    def run(self, command: dict, db: str = DB) -> dict:
        return op_msg(self.address, {**command, "$db": db})

    def _size(self) -> int:
        outer = 1_000
        while True:
            self.seed(1, outer=outer)
            began = time.monotonic()
            self.run({"find": "c", "filter": SLOW})
            took = time.monotonic() - began
            if took > 0.2:
                return int(outer * 2.0 / took)
            outer *= 10

    def seed(self, n: int, per_doc: float = 2.0, *, outer: int | None = None) -> None:
        """`n` documents, each taking about `per_doc` seconds to test."""
        self.run({"drop": "c"})
        outer = outer if outer is not None else max(1, int(self.outer * per_doc / 2.0))
        docs = [{"_id": i, "n": 0, "outer": outer, "inner": 1000} for i in range(n)]
        self.run({"insert": "c", "documents": docs})

    def state(self) -> tuple:
        docs = self.run({"find": "c", "batchSize": 10_000})["cursor"]["firstBatch"]
        return len(docs), sum(d.get("n", 0) for d in docs)

    def kill(self, how: str, lsid: dict | None, match: dict) -> dict:
        if how == "killOp":
            ops = self.run(
                {"aggregate": 1, "pipeline": [{"$currentOp": {}}, {"$match": match}], "cursor": {}},
                db="admin",
            )["cursor"]["firstBatch"]
            assert ops, f"no running operation matched {match}"
            return self.run({"killOp": 1, "op": ops[0]["opid"]}, db="admin")
        if how == "killSessions":
            return self.run({"killSessions": [lsid]}, db="admin")
        if how == "killAllSessions":
            return self.run({"killAllSessions": []}, db="admin")
        if how == "killAllSessionsByPattern":
            # A pattern's `lsid` names its owner too: here no user, whose
            # `uid` is the SHA-256 of nothing.
            assert lsid is not None
            uid = Binary(hashlib.sha256(b"").digest(), 0)
            pattern = {"lsid": {**lsid, "uid": uid}}
            return self.run({"killAllSessionsByPattern": [pattern]}, db="admin")
        if how == "endSessions":
            return self.run({"endSessions": [lsid]}, db="admin")
        raise ValueError(how)

    def killed(
        self,
        command: dict,
        how: str,
        *,
        lsid: dict | None = None,
        match: dict | None = None,
        db: str = DB,
    ) -> tuple:
        """Run `command`, `how` it 0.3 s in, and return both replies."""
        replies: list = []
        thread = threading.Thread(
            target=lambda: replies.append(self.run(command, db=db)), daemon=True
        )
        thread.start()
        time.sleep(0.3)
        kill = self.kill(how, lsid, match or {"ns": f"{DB}.c"})
        thread.join(120)
        assert replies, "the operation never answered"
        return clean(replies[0]), clean(kill)


def _lsid() -> dict:
    return {"id": Binary(uuid.uuid4().bytes, 4)}


def _killed_update(client: Any, how: str) -> tuple:
    slow = Slow(client)
    slow.seed(1)
    lsid = _lsid()
    command = {
        "update": "c",
        "updates": [{"q": SLOW, "u": {"$inc": {"n": 1}}}],
        "lsid": lsid,
        "txnNumber": Int64(1),
    }
    reply, kill = slow.killed(command, how, lsid=lsid)
    applied = slow.state()
    # The retry of the killed statement runs it: nothing was recorded.
    retry = clean(slow.run({**command, "updates": [{"q": {"_id": 0}, "u": {"$inc": {"n": 1}}}]}))
    return reply, kill, applied, retry


def test_a_kill_interrupts_a_running_retryable_update(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 interrupts a retryable update whatever names it --
    `killOp`, `killSessions`, `killAllSessions`, a pattern -- with 11601
    under the update's executor wrapper and a `writeConcernError`, applies
    nothing, and runs the retry; `endSessions` interrupts nothing."""
    for how in ("killOp", "killSessions", "killAllSessions", "killAllSessionsByPattern"):
        theirs = _killed_update(mongod_rs, how)
        assert theirs[0]["code"] == 11601 and theirs[2] == (1, 0), (how, theirs)
        same(_killed_update(rust_mongo, how), theirs)
    theirs = _killed_update(mongod_rs, "endSessions")
    assert theirs[0]["ok"] == 1.0 and theirs[2] == (1, 1), theirs
    ours = _killed_update(rust_mongo, "endSessions")
    same(ours[0], theirs[0])
    assert ours[2] == theirs[2]


def _killed_command(client: Any, command: dict, how: str = "killSessions") -> tuple:
    slow = Slow(client)
    slow.seed(1)
    lsid = _lsid()
    reply, _ = slow.killed({**command, "lsid": lsid}, how, lsid=lsid)
    return reply, slow.state()


_COMMANDS = {
    "find": {"find": "c", "filter": SLOW},
    "aggregate": {"aggregate": "c", "pipeline": [{"$project": {"x": SLOW_EXPR}}], "cursor": {}},
    "count": {"count": "c", "query": SLOW},
    "distinct": {"distinct": "c", "key": "n", "query": SLOW},
    "delete": {"delete": "c", "deletes": [{"q": SLOW, "limit": 1}]},
    "findAndModify": {"findAndModify": "c", "query": SLOW, "update": {"$inc": {"n": 1}}},
    "findAndModify remove": {"findAndModify": "c", "query": SLOW, "remove": True},
}


def test_each_command_answers_its_interruption(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11: a read's 11601 goes under its executor wrapper, a
    delete's under `Plan executor error during delete` with a
    `writeConcernError`, and `findAndModify`'s is bare, with none; nothing
    is written."""
    for name, command in _COMMANDS.items():
        theirs = _killed_command(mongod_rs, command)
        assert theirs[0]["code"] == 11601 and theirs[1] == (1, 0), (name, theirs)
        same(_killed_command(rust_mongo, command), theirs)


def _killed_batch(client: Any, ordered: bool) -> tuple:
    """Fifty one-document statements; the kill lands part-way."""
    slow = Slow(client)
    slow.seed(50, 0.04)
    lsid = _lsid()
    statements = [{"q": {"_id": i, **SLOW}, "u": {"$inc": {"n": 1}}} for i in range(50)]
    command = {
        "update": "c",
        "updates": statements,
        "ordered": ordered,
        "lsid": lsid,
        "txnNumber": Int64(5),
    }
    reply, _ = slow.killed(command, "killSessions", lsid=lsid)
    count, applied = slow.state()
    retry = slow.run(command)
    # Where the kill lands decides the wrapper, on mongod as here: inside a
    # statement's filter it is "Plan executor error during update :: caused
    # by :: operation was interrupted", between two statements the bare
    # message. Both are mongod's answer.
    wrapper = "Plan executor error during update :: caused by :: "
    assert reply["errmsg"] in ("operation was interrupted", wrapper + "operation was interrupted")
    reply["errmsg"] = reply["errmsg"].removeprefix(wrapper)
    # What matters, not when the kill landed: the statements before it are
    # applied, the ones after not, and a retry answers for exactly those.
    return (
        reply,
        0 < applied < 50,
        retry.get("retriedStmtIds") == list(range(applied)),
        slow.state() == (50, 50),
    )


def test_a_killed_batch_keeps_the_statements_before(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 fails the whole batch when one statement is killed,
    ordered or not, keeps the statements before it applied and recorded,
    and a retry answers for those and runs the rest."""
    for ordered in (True, False):
        theirs = _killed_batch(mongod_rs, ordered)
        assert theirs[1:] == (True, True, True), theirs
        same(_killed_batch(rust_mongo, ordered), theirs)


def _killed_bulk_write(client: Any, ordered: bool) -> tuple:
    """A `bulkWrite` of fifty one-document updates; the kill lands
    part-way."""
    slow = Slow(client)
    slow.seed(50, 0.04)
    lsid = _lsid()
    ops = [
        {"update": 0, "filter": {"_id": i, **SLOW}, "updateMods": {"$inc": {"n": 1}}}
        for i in range(50)
    ]
    command = {
        "bulkWrite": 1,
        "ops": ops,
        "nsInfo": [{"ns": f"{DB}.c"}],
        "ordered": ordered,
        "lsid": lsid,
    }
    reply, _ = slow.killed(command, "killSessions", lsid=lsid, db="admin")
    _, applied = slow.state()
    # As for `update` (above), where the kill lands decides the wrapper.
    wrapper = "Plan executor error during update :: caused by :: "
    assert reply["errmsg"] in ("operation was interrupted", wrapper + "operation was interrupted")
    reply["errmsg"] = reply["errmsg"].removeprefix(wrapper)
    return reply, 0 < applied < 50


def test_a_killed_bulk_write_fails_the_command(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 fails a `bulkWrite` whose op is killed, ordered or not,
    as `update` does (11601 and a `writeConcernError`, no cursor), and the
    ops before it stay applied. This server reported the op's failure in
    the cursor and, unordered, ran the ops after it."""
    for ordered in (True, False):
        theirs = _killed_bulk_write(mongod_rs, ordered)
        assert theirs[0]["code"] == 11601 and theirs[1], theirs
        same(_killed_bulk_write(rust_mongo, ordered), theirs)


def _killed_in_transaction(client: Any, how: str) -> tuple:
    slow = Slow(client)
    slow.seed(1)
    lsid = _lsid()
    txn = {"lsid": lsid, "txnNumber": Int64(1), "autocommit": False}
    first = clean(
        slow.run({"insert": "c", "documents": [{"_id": 99}], "startTransaction": True, **txn})
    )
    command = {"update": "c", "updates": [{"q": SLOW, "u": {"$inc": {"n": 1}}}], **txn}
    reply, _ = slow.killed(command, how, lsid=lsid, match={"ns": f"{DB}.c", "op": "update"})
    commit = clean(slow.run({"commitTransaction": 1, **txn}, db="admin"))
    return first, reply, commit, slow.state()


def test_a_kill_aborts_the_transaction(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11: a transaction's statement killed by `killOp` or
    `killSessions` answers 11601 with no `writeConcernError`, and the
    transaction is aborted (251 with `TransientTransactionError` at commit,
    its first write gone). `killSessions` does not wait for the statement."""
    for how in ("killOp", "killSessions"):
        theirs = _killed_in_transaction(mongod_rs, how)
        assert theirs[1]["code"] == 11601 and theirs[2]["code"] == 251, theirs
        same(_killed_in_transaction(rust_mongo, how), theirs)


def _killed_get_more(client: Any, how: str) -> tuple:
    slow = Slow(client)
    slow.seed(5, 1.0)
    lsid = _lsid()
    opened = slow.run({"find": "c", "filter": SLOW, "batchSize": 1, "lsid": lsid})
    cursor = opened["cursor"]["id"]
    get_more = {"getMore": cursor, "collection": "c", "lsid": lsid}
    reply, _ = slow.killed(get_more, how, lsid=lsid, match={"op": "getmore"})
    after = clean(slow.run(get_more))
    after["errmsg"] = after.get("errmsg", "").replace(str(cursor), "N")
    return reply, after


def test_a_kill_ends_a_running_get_more_and_its_cursor(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 interrupts a getMore running on a session it kills, or
    one `killOp` names, and the cursor is gone (43) afterwards."""
    for how in ("killSessions", "killOp"):
        theirs = _killed_get_more(mongod_rs, how)
        assert theirs[0]["code"] == 11601 and theirs[1]["code"] == 43, theirs
        same(_killed_get_more(rust_mongo, how), theirs)


def _killed_insert(client: Any, retryable: bool) -> tuple:
    """Forty documents through a slow `$expr` validator, which no interrupt
    reaches on mongod."""
    slow = Slow(client)
    slow.run({"drop": "v"})
    slow.run({"create": "v", "validator": SLOW})
    outer = max(1, int(slow.outer * 0.05 / 2.0))
    docs = [{"_id": i, "outer": outer, "inner": 1000} for i in range(40)]
    lsid = _lsid()
    command = {"insert": "v", "documents": docs, "lsid": lsid}
    if retryable:
        command["txnNumber"] = Int64(3)
    reply, _ = slow.killed(command, "killSessions", lsid=lsid, match={"ns": f"{DB}.v"})
    return reply, slow.run({"count": "v"})["n"]


def test_a_killed_insert_finishes_unless_retryable(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 does not interrupt an insert in its validator: a plain
    insert inserts every document and carries the interrupted write-concern
    wait; a retryable one checks before its batch commits, and inserts
    none."""
    for retryable in (False, True):
        theirs = _killed_insert(mongod_rs, retryable)
        assert theirs[1] == (0 if retryable else 40), theirs
        same(_killed_insert(rust_mongo, retryable), theirs)


def _killed_with_write_concern(client: Any, write_concern: dict | None) -> dict:
    slow = Slow(client)
    slow.seed(1)
    command: dict = {"update": "c", "updates": [{"q": SLOW, "u": {"$inc": {"n": 1}}}]}
    if write_concern is not None:
        command["writeConcern"] = write_concern
    reply, _ = slow.killed(command, "killOp", match={"ns": f"{DB}.c", "op": "update"})
    return reply


def test_the_write_concern_error_follows_the_wait(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11: a killed write carries a `writeConcernError` when its
    write concern waits (the default, `majority`, `j`, `fsync`) and none
    for `w: 1`, which waits for nothing; it names `{w: 1, wtimeout: 0}`."""
    for write_concern in (
        None,
        {"w": 1},
        {"w": "majority"},
        {"w": 1, "j": True},
        {"w": 1, "j": False},
    ):
        theirs = _killed_with_write_concern(mongod_rs, write_concern)
        same(_killed_with_write_concern(rust_mongo, write_concern), theirs)


def _out_of_time(client: Any) -> list:
    slow = Slow(client)
    out = []
    for command in (
        {"update": "c", "updates": [{"q": SLOW, "u": {"$inc": {"n": 1}}}]},
        {"delete": "c", "deletes": [{"q": SLOW, "limit": 1}]},
        {"findAndModify": "c", "query": SLOW, "update": {"$inc": {"n": 1}}},
        {"find": "c", "filter": SLOW},
    ):
        slow.seed(1)
        out.append((clean(slow.run({**command, "maxTimeMS": 300})), slow.state()))
    return out


def test_max_time_ms_ends_a_long_expression(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 ends an operation whose `maxTimeMS` runs out inside
    one document's expression, with 50 under the same wrappers a kill takes
    (and the same `writeConcernError`), writing nothing."""
    theirs = _out_of_time(mongod_rs)
    assert all(reply["code"] == 50 for reply, _ in theirs), theirs
    same(_out_of_time(rust_mongo), theirs)


def _killed_waiting_for_a_lock(client: Any, command: dict, how: str) -> tuple:
    """`command` waits for the collection a transaction wrote to; kill it
    while it waits, then end the transaction."""
    slow = Slow(client)
    slow.run({"dropDatabase": 1})
    slow.run({"insert": "c", "documents": [{"_id": 1}]})
    txn = {"lsid": _lsid(), "txnNumber": Int64(1), "autocommit": False}
    slow.run({"insert": "c", "documents": [{"_id": 2}], "startTransaction": True, **txn})
    lsid = _lsid()
    name = next(iter(command))
    reply, _ = slow.killed(
        {**command, "lsid": lsid}, how, lsid=lsid, match={f"command.{name}": "c"}
    )
    slow.run({"abortTransaction": 1, **txn}, db="admin")
    return reply, slow.run({"count": "c"})["n"]


def test_a_kill_ends_a_lock_wait(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 interrupts a `drop` or `createIndexes` waiting for a
    transaction's lock with a bare 11601, `createIndexes` with a
    `writeConcernError` and `drop` without; this server waited on."""
    for command in (
        {"drop": "c"},
        {"createIndexes": "c", "indexes": [{"key": {"x": 1}, "name": "x_1"}]},
    ):
        for how in ("killOp", "killSessions"):
            theirs = _killed_waiting_for_a_lock(mongod_rs, command, how)
            assert theirs[0]["code"] == 11601 and theirs[1] == 1, (command, how, theirs)
            same(_killed_waiting_for_a_lock(rust_mongo, command, how), theirs)
