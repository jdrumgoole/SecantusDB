"""Gap tests for the divergences found on 2026-10-27 while making a kill
interrupt the operation it names (RM-313): what a multi-document write that
stops part-way leaves written.

The reference is mongod 8.2.11, asked live (`mongod_rs`, a one-member
replica set); the Rust server is `secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import threading
import time
import uuid
from typing import Any

import pytest
from bson import Binary

from tests.backlog.conftest import mongo_answer
from tests.mongod_live.test_interrupts import SLOW, Slow


def _fails_part_way(client: Any) -> tuple:
    """An updateMany whose filter divides by zero on the third of five
    documents: its answer, and which documents it updated."""
    db = client["rm322"]
    db.drop_collection("c")
    db.c.insert_many([{"_id": i, "d": 0 if i == 2 else 1, "n": 0} for i in range(5)])
    command = {
        "update": "c",
        "updates": [
            {
                "q": {"$expr": {"$gt": [{"$divide": [1, "$d"]}, 0]}},
                "u": {"$inc": {"n": 1}},
                "multi": True,
            }
        ],
    }
    reply = mongo_answer(lambda: db.command(command))
    if reply[0] == "ok":
        reply = (
            "ok",
            {k: v for k, v in reply[1].items() if k in ("n", "nModified", "writeErrors")},
        )
    return reply, sorted((d["_id"], d["n"]) for d in db.c.find())


@pytest.mark.backlog_fixed("RM-322")
def test_an_update_many_failing_part_way_keeps_the_documents_before(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 updates each document as its scan reaches it, so an
    updateMany whose filter fails on the third of five documents has
    updated the first two, though it answers `n: 0` with the error. This
    server tested every document before writing any, and updated none."""
    theirs = _fails_part_way(mongod_rs)
    assert theirs[1] == [(0, 1), (1, 1), (2, 0), (3, 0), (4, 0)], theirs
    assert _fails_part_way(rust_mongo) == theirs


def _delete_fails_at(client: Any, at: int) -> tuple:
    """A deleteMany of 40 documents whose filter divides by zero on
    document `at`: its error codes, and how many documents are left."""
    db = client["rm322"]
    db.drop_collection("c")
    db.c.insert_many([{"_id": i, "d": 0 if i == at else 1} for i in range(40)])
    command = {
        "delete": "c",
        "deletes": [{"q": {"$expr": {"$gt": [{"$divide": [1, "$d"]}, 0]}}, "limit": 0}],
    }
    reply = db.command(command)
    return [e["code"] for e in reply.get("writeErrors", [])], db.c.count_documents({})


@pytest.mark.backlog_fixed("RM-322")
def test_a_delete_many_failing_part_way_keeps_the_batches_before(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 deletes a deleteMany's documents ten at a time as its
    scan finds them, so one whose filter fails on the 25th document has
    deleted the first 20, on the 11th the first 10, and on the 10th none.
    This server tested every document before deleting any, and deleted
    none."""
    for at, left in ((24, 20), (10, 30), (9, 40)):
        theirs = _delete_fails_at(mongod_rs, at)
        assert theirs == ([2], left), (at, theirs)
        assert _delete_fails_at(rust_mongo, at) == theirs, at


def _delete_many_killed(client: Any) -> tuple:
    """A deleteMany of 100 slow documents, its session killed 1.5 s in:
    its answer, and whether it had deleted any."""
    slow = Slow(client)
    slow.seed(100, 0.04)
    lsid = {"id": Binary(uuid.uuid4().bytes, 4)}
    command = {"delete": "c", "deletes": [{"q": SLOW, "limit": 0}], "lsid": lsid}
    replies: list = []
    thread = threading.Thread(target=lambda: replies.append(slow.run(command)), daemon=True)
    thread.start()
    time.sleep(1.5)
    slow.kill("killSessions", lsid, {})
    thread.join(120)
    assert replies, "the delete never answered"
    left, _ = slow.state()
    return replies[0].get("code"), left < 100


@pytest.mark.backlog("RM-323")
def test_a_killed_delete_many_keeps_the_batches_it_deleted(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 deletes a deleteMany's documents a batch at a time as
    its scan finds them, so one killed 1.5 s into 100 slow documents has
    deleted some (20, measured 2026-10-27). On a replica set this server
    first works out mongod's order, testing every document, and a kill
    there leaves nothing deleted."""
    theirs = _delete_many_killed(mongod_rs)
    assert theirs == (11601, True), theirs
    assert _delete_many_killed(rust_mongo) == theirs


def _update_many_out_of_time(client: Any) -> tuple:
    """An updateMany of 100,000 plain documents with `maxTimeMS: 30`: its
    error code, and whether it had updated any."""
    db = client["rm323"]
    db.drop_collection("c")
    for start in range(0, 100_000, 10_000):
        db.c.insert_many([{"_id": i, "k": i % 7, "n": 0} for i in range(start, start + 10_000)])
    command = {
        "update": "c",
        "updates": [{"q": {"k": {"$gte": 0}}, "u": {"$inc": {"n": 1}}, "multi": True}],
        "maxTimeMS": 30,
    }
    answer = mongo_answer(lambda: db.command(command))
    updated = db.c.count_documents({"n": 1})
    return answer[:2], 0 < updated < 100_000


@pytest.mark.backlog("RM-323")
def test_an_update_many_out_of_time_keeps_the_documents_it_updated(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 updates documents as its scan reads them, so an
    updateMany whose `maxTimeMS` runs out part-way has updated some (1,536
    of 200,000 at 5 ms, measured 2026-10-27). This server reads every
    candidate before it tests or writes one, and a budget spent in that
    read leaves nothing updated."""
    theirs = _update_many_out_of_time(mongod_rs)
    assert theirs == (("err", 50), True), theirs
    assert _update_many_out_of_time(rust_mongo) == theirs
