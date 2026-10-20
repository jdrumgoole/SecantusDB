"""RM-219: `setUserWriteBlockMode`, measured against mongod 8.2.11.

mongod refuses a write to any database but `admin`, `config` and `local`
with 371 UserWritesBlocked at the point the write would happen, so a command
that changes nothing still succeeds. The block is kept in
`config.user_writes_critical_sections` and lifted only with the reason it was
set with.
"""

from __future__ import annotations

import re
import subprocess
from collections.abc import Callable
from typing import Any

import pymongo
import pytest
from pymongo.errors import PyMongoError

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output
from tests.backlog.test_rm_057_084 import auth_rust, mongod_rs  # noqa: F401 - fixture

UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
SKIP = ("$clusterTime", "operationTime", "electionId", "opTime", "commitQuorum")


def _answer(fn: Callable[[], Any]) -> tuple:
    """`("ok", reply)` or `("err", code, message)`, with the index-build
    UUIDs (fresh each run) replaced."""
    try:
        got = fn()
        if isinstance(got, dict):
            got = {k: v for k, v in got.items() if k not in SKIP}
        return ("ok", UUID.sub("<uuid>", repr(got)))
    except PyMongoError as e:
        details = getattr(e, "details", None) or {}
        return ("err", getattr(e, "code", None), UUID.sub("<uuid>", str(details.get("errmsg"))))


def _blocked_writes(client: Any) -> dict:
    admin, db = client.admin, client.rm219w
    client.drop_database("rm219w")
    db.c.insert_many([{"_id": i, "x": i} for i in range(3)])
    db.c.create_index("x")
    db.command("create", "v0", viewOn="c", pipeline=[])
    out: dict = {}

    def run(label: str, fn: Callable[[], Any]) -> None:
        out[label] = _answer(fn)

    run("no global", lambda: admin.command({"setUserWriteBlockMode": 1}))
    run(
        "bad reason",
        lambda: admin.command({"setUserWriteBlockMode": 1, "global": False, "reason": "x"}),
    )
    run(
        "unknown field",
        lambda: admin.command({"setUserWriteBlockMode": 1, "global": False, "x": 1}),
    )
    run("block", lambda: admin.command({"setUserWriteBlockMode": 1, "global": True}))
    try:
        run("stored", lambda: list(client.config.user_writes_critical_sections.find()))
        run(
            "block, other reason",
            lambda: admin.command(
                {"setUserWriteBlockMode": 1, "global": True, "reason": "DiskUseThresholdExceeded"}
            ),
        )
        run(
            "insert",
            lambda: db.command("insert", "c", documents=[{"_id": 10}, {"_id": 11}], ordered=False),
        )
        run("insert, new collection", lambda: db.command("insert", "new", documents=[{"_id": 1}]))
        run(
            "update",
            lambda: db.command("update", "c", updates=[{"q": {"_id": 1}, "u": {"$set": {"y": 1}}}]),
        )
        run(
            "update, no match",
            lambda: db.command(
                "update", "c", updates=[{"q": {"_id": 99}, "u": {"$set": {"y": 1}}}]
            ),
        )
        run(
            "upsert",
            lambda: db.command(
                "update", "c", updates=[{"q": {"_id": 50}, "u": {"$set": {"y": 1}}, "upsert": True}]
            ),
        )
        run("delete", lambda: db.command("delete", "c", deletes=[{"q": {"_id": 1}, "limit": 1}]))
        run(
            "findAndModify",
            lambda: db.command("findAndModify", "c", query={"_id": 1}, update={"$set": {"y": 1}}),
        )
        run(
            "new index",
            lambda: db.command("createIndexes", "c", indexes=[{"key": {"y": 1}, "name": "y_1"}]),
        )
        run(
            "existing index",
            lambda: db.command("createIndexes", "c", indexes=[{"key": {"x": 1}, "name": "x_1"}]),
        )
        run("dropIndexes", lambda: db.command("dropIndexes", "c", index="x_1"))
        run("collMod", lambda: db.command("collMod", "c"))
        run("create view", lambda: db.command("create", "v", viewOn="c", pipeline=[]))
        run("drop", lambda: db.command("drop", "c"))
        run("drop, missing", lambda: db.command("drop", "nope"))
        run("dropDatabase, missing", lambda: client.rm219w_none.command("dropDatabase"))
        run("rename", lambda: admin.command("renameCollection", "rm219w.c", to="rm219w.d"))
        run("$out", lambda: db.command("aggregate", "c", pipeline=[{"$out": "o"}], cursor={}))
        run("$merge", lambda: db.command("aggregate", "c", pipeline=[{"$merge": "m"}], cursor={}))
        run("config", lambda: client.config.rm219w.insert_one({"_id": 1}).inserted_id)
        run("find", lambda: list(db.c.find({}, {"_id": 1})))
        run(
            "unblock, other reason",
            lambda: admin.command(
                {"setUserWriteBlockMode": 1, "global": False, "reason": "DiskUseThresholdExceeded"}
            ),
        )
    finally:
        run("unblock", lambda: admin.command({"setUserWriteBlockMode": 1, "global": False}))
    run("stored after", lambda: list(client.config.user_writes_critical_sections.find()))
    run("insert after", lambda: db.command("insert", "c", documents=[{"_id": 10}]))
    client.config.rm219w.drop()
    client.drop_database("rm219w")
    return out


@pytest.mark.backlog_fixed("RM-219")
def test_user_write_block_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """Each write refused, or let through because it changes nothing, with
    mongod 8.2.11's code and message (the insert's per document, the update's
    and delete's under the update executor, `$out`'s under the aggregate
    executor, a new index's under the index build)."""
    assert _blocked_writes(rust_mongo) == _blocked_writes(mongod_rs)


def _no_op_drop_database_event(client: Any) -> str:
    """The first event a cluster stream sees after a database that does not
    exist is dropped and a document is inserted elsewhere."""
    client.drop_database("rm219w_gone")
    with client.watch() as stream:
        client.rm219w_gone.command("dropDatabase")
        client.rm219w_ev.c.insert_one({"_id": 1})
        event = stream.next()
    client.drop_database("rm219w_ev")
    return event["operationType"]


@pytest.mark.backlog_fixed("RM-219")
def test_dropping_a_missing_database_writes_no_event(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 writes no oplog entry for dropping a database that does
    not exist, so a change stream sees none (and a user write block does not
    refuse it)."""
    assert _no_op_drop_database_event(mongod_rs) == "insert"
    assert _no_op_drop_database_event(rust_mongo) == "insert"


def _start(store: Any) -> tuple[subprocess.Popen, Any]:
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(store)],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    assert proc.stdout is not None
    m = _RS_BANNER.search(proc.stdout.readline())
    assert m, "secantusd-rs did not start"
    drain_output(proc)
    client = pymongo.MongoClient(host=m.group(1), port=int(m.group(2)), directConnection=True)
    return proc, client


def _stop(proc: subprocess.Popen, client: Any) -> None:
    client.close()
    proc.terminate()
    proc.wait(timeout=20)


@pytest.mark.backlog_fixed("RM-219")
def test_user_write_block_survives_a_restart(tmp_path: Any) -> None:
    """The block is the document mongod keeps in
    `config.user_writes_critical_sections`, so a restarted server is still
    blocked until it is lifted."""
    store = tmp_path / "store"
    proc, client = _start(store)
    try:
        client.admin.command(
            {"setUserWriteBlockMode": 1, "global": True, "reason": "DiskUseThresholdExceeded"}
        )
    finally:
        _stop(proc, client)
    proc, client = _start(store)
    try:
        got = client.rm219w.command("insert", "c", documents=[{"_id": 1}])["writeErrors"]
        assert got == [
            {
                "index": 0,
                "code": 371,
                "errmsg": "User writes blocked, reason: DiskUseThresholdExceeded",
            }
        ]
        client.admin.command(
            {"setUserWriteBlockMode": 1, "global": False, "reason": "DiskUseThresholdExceeded"}
        )
        assert client.rm219w.command("insert", "c", documents=[{"_id": 1}])["n"] == 1
    finally:
        _stop(proc, client)


@pytest.mark.backlog_fixed("RM-219")
def test_bypass_write_blocking_mode_writes_past_the_block(tmp_path: Any) -> None:
    """In mongod 8.2.11's built-in roles `root` holds `bypassWriteBlockingMode`
    and `readWriteAnyDatabase` does not: under `--auth` the first writes past
    a block and the second is refused."""
    with auth_rust(tmp_path) as (host, port):
        root = pymongo.MongoClient(
            host=host, port=port, directConnection=True, username="root", password="pw"
        )
        alice = pymongo.MongoClient(
            host=host, port=port, directConnection=True, username="alice", password="pw"
        )
        root.admin.command({"setUserWriteBlockMode": 1, "global": True})
        try:
            refused = alice.rm219w.command("insert", "c", documents=[{"_id": 1}])
            written = root.rm219w.command("insert", "c", documents=[{"_id": 2}])
        finally:
            root.admin.command({"setUserWriteBlockMode": 1, "global": False})
    assert refused["writeErrors"][0]["code"] == 371
    assert written["n"] == 1 and "writeErrors" not in written
