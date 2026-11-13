"""Gap tests for what RM-390 left open on 2026-11-10: a write of many whose
plan reads an index, or whose plan the planner has to choose, that runs out
of `maxTimeMS` before it has written anything.

The store's own candidates are now read a page at a time and each page
written before the next is read, so a collection scan out of time keeps
what it wrote. A contested plan is now tried over what its candidates
reach, and the winner streamed into the write (2026-11-13). The router's
RecordIds of an index, read for a clustered collection, still come whole
before the first write.

The reference is mongod 8.2.11, asked live (`mongod_rs`, a one-member
replica set); the Rust server is `secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

DOCS = 100_000


def _write_out_of_time(client: Any, plan: str, op: str, budget: int = 30) -> tuple:
    """An updateMany (`$inc: {n: 1}`) or a deleteMany of 100,000 documents
    with `maxTimeMS: budget`, its read `plan`: `clustered-index` reads index
    `a_1` of a collection clustered by `_id`, `contested` has indexes `a_1`
    and `b_1` both able to answer it. Its error code, and whether it had
    written some documents but not all."""
    db = client["rm390"]
    db.drop_collection("c")
    if plan == "clustered-index":
        db.create_collection("c", clusteredIndex={"key": {"_id": 1}, "unique": True})
    for start in range(0, DOCS, 10_000):
        db.c.insert_many(
            [{"_id": i, "a": i, "b": i % 7, "n": 0} for i in range(start, start + 10_000)]
        )
    db.c.create_index("a")
    if plan == "contested":
        db.c.create_index("b")
        query = {"a": {"$gte": 0}, "b": {"$gte": 0}}
    else:
        query = {"a": {"$gte": 0}}
    if op == "update":
        command = {
            "update": "c",
            "updates": [{"q": query, "u": {"$inc": {"n": 1}}, "multi": True}],
            "maxTimeMS": budget,
        }
    else:
        command = {"delete": "c", "deletes": [{"q": query, "limit": 0}], "maxTimeMS": budget}
    answer = mongo_answer(lambda: db.command(command))
    left = db.c.count_documents({"n": 0} if op == "update" else {})
    written = DOCS - left
    return answer[:2], 0 < written < DOCS


@pytest.mark.backlog("RM-390")
@pytest.mark.parametrize("op", ["update", "delete"])
def test_a_write_of_many_out_of_time_keeps_what_it_wrote_whatever_its_plan(
    rust_mongo: Any, mongod_rs: Any, op: str
) -> None:
    """mongod 8.2.11 writes each document as its plan returns it, so an
    updateMany or deleteMany out of `maxTimeMS` part-way has written some,
    whatever the plan (measured 2026-11-10: through index `a_1` of a
    clustered collection 4,599 updated and 11,280 deleted of 100,000). This
    server reads the index's every RecordId before it writes one, and a
    budget spent there writes nothing."""
    theirs = _write_out_of_time(mongod_rs, "clustered-index", op)
    assert theirs == (("err", 50), True), theirs
    assert _write_out_of_time(rust_mongo, "clustered-index", op) == theirs


@pytest.mark.backlog_fixed("RM-390")
@pytest.mark.parametrize("op", ["update", "delete"])
def test_a_contested_write_of_many_out_of_time_keeps_what_it_wrote(
    rust_mongo: Any, mongod_rs: Any, op: str
) -> None:
    """mongod 8.2.11 tries a contested plan over what its candidates reach
    and then writes each document the winner returns, so an updateMany or
    deleteMany out of `maxTimeMS` part-way has written some (measured
    2026-11-10: 1,536 updated and 10,030 deleted of 100,000). This server
    read every match to choose the plan before it wrote one, and a budget
    spent there wrote nothing; it now tries the plan as mongod does and
    streams the winner into the write."""
    # 150 ms, not the 30 the gap was measured with: the plan trial before a
    # streaming write's first document took about 30 ms in this server's
    # debug build here and more on CI's runner, where at 30 ms it wrote
    # nothing (PR #2722, 2026-11-13). Reading every match before the first
    # write, the behaviour this guards against, costs about 1.25 s in that
    # build, so 150 ms still tells the two apart; mongod 8.2.11 stops
    # part-way at 150 ms too.
    theirs = _write_out_of_time(mongod_rs, "contested", op, budget=150)
    assert theirs == (("err", 50), True), theirs
    assert _write_out_of_time(rust_mongo, "contested", op, budget=150) == theirs
