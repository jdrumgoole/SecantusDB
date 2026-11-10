"""Gap tests for what RM-390 left open on 2026-11-10: a write of many whose
plan reads an index, or whose plan the planner has to choose, that runs out
of `maxTimeMS` before it has written anything.

The store's own candidates are now read a page at a time and each page
written before the next is read, so a collection scan out of time keeps
what it wrote. The reads below still happen whole before the first write:
the router's RecordIds of an index, and the read that picks a contested
plan (`explain::order::through_cache`, which runs `ordered_rows` over
every match).

The reference is mongod 8.2.11, asked live (`mongod_rs`, a one-member
replica set); the Rust server is `secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

DOCS = 100_000


def _write_out_of_time(client: Any, plan: str, op: str) -> tuple:
    """An updateMany (`$inc: {n: 1}`) or a deleteMany of 100,000 documents
    with `maxTimeMS: 30`, its read `plan`: `clustered-index` reads index
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
            "maxTimeMS": 30,
        }
    else:
        command = {"delete": "c", "deletes": [{"q": query, "limit": 0}], "maxTimeMS": 30}
    answer = mongo_answer(lambda: db.command(command))
    left = db.c.count_documents({"n": 0} if op == "update" else {})
    written = DOCS - left
    return answer[:2], 0 < written < DOCS


@pytest.mark.backlog("RM-390")
@pytest.mark.parametrize(
    "plan,op",
    [
        ("clustered-index", "update"),
        ("clustered-index", "delete"),
        ("contested", "update"),
        ("contested", "delete"),
    ],
)
def test_a_write_of_many_out_of_time_keeps_what_it_wrote_whatever_its_plan(
    rust_mongo: Any, mongod_rs: Any, plan: str, op: str
) -> None:
    """mongod 8.2.11 writes each document as its plan returns it, so an
    updateMany or deleteMany out of `maxTimeMS` part-way has written some,
    whatever the plan (measured 2026-11-10: through index `a_1` of a
    clustered collection 4,599 updated and 11,280 deleted of 100,000; a
    contested plan 1,536 updated and 10,030 deleted). This server reads the
    index's every RecordId, or every match of a contested plan to choose
    it, before it writes one, and a budget spent there writes nothing."""
    theirs = _write_out_of_time(mongod_rs, plan, op)
    assert theirs == (("err", 50), True), theirs
    assert _write_out_of_time(rust_mongo, plan, op) == theirs
