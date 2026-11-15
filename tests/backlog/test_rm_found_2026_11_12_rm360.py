"""Gap tests for items found while working RM-360 (an aggregate's
interrupt checks), 2026-11-12, against mongod 8.2.11 (a one-member replica
set, macOS arm64)."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.opdebug import logged, no_timed_yields


def _stages(client: Any, pipeline: list) -> list:
    """The pipeline's explained stages: the `$cursor`'s parsed query, then
    each stage after it as listed."""
    client.drop_database("rm_found_redact")
    db = client["rm_found_redact"]
    db.c.insert_many([{"_id": n, "z": n % 2} for n in range(6)])
    reply = db.command(
        "explain", {"aggregate": "c", "pipeline": pipeline, "cursor": {}}, verbosity="queryPlanner"
    )
    stages = reply["stages"]
    return [{"$cursor": stages[0]["$cursor"]["queryPlanner"]["parsedQuery"]}, *stages[1:]]


@pytest.mark.backlog_fixed("RM-580")
def test_a_match_after_a_redact_is_copied_ahead_of_it(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 copies a `$match` that follows a `$redact` ahead of it
    and into the query it reads with; this server listed the two stages as
    written over a whole-collection read, until RM-580."""
    pipeline = [{"$redact": "$$KEEP"}, {"$match": {"z": 0}}]
    ours, theirs = _stages(rust_mongo, pipeline), _stages(mongod_rs, pipeline)
    assert theirs == [
        {"$cursor": {"z": {"$eq": 0}}},
        {"$match": {"z": {"$eq": 0}}},
        {"$redact": "$$KEEP"},
        {"$match": {"z": {"$eq": 0}}},
    ]
    assert ours == theirs


@pytest.mark.backlog_fixed("RM-580")
@pytest.mark.parametrize(
    "pipeline",
    [
        # Behind a stage the copy passes: read, and listed ahead of `$redact`.
        [{"$addFields": {"q": 1}}, {"$redact": "$$KEEP"}, {"$match": {"z": 0}}],
        # Only the safe part: not `$exists`, null, `$ne` or `$size`.
        [
            {"$redact": "$$KEEP"},
            {
                "$match": {
                    "z": {"$in": [0, 1]},
                    "w": None,
                    "v": {"$exists": False},
                    "u": {"$exists": True},
                    "n": {"$size": 1},
                    "t": {"$ne": 1},
                }
            },
        ],
        # Null is not safe: nothing copied. (An `$or` is left out: mongod
        # 8.2.11 lists its branches in an order that varies run to run.)
        [{"$redact": "$$KEEP"}, {"$match": {"z": None}}],
        # `$type`, `$regex`, `$mod`, `$elemMatch` and a dotted path are safe.
        [
            {"$redact": "$$KEEP"},
            {
                "$match": {
                    "z": {"$type": "int"},
                    "s": {"$regex": "a", "$options": "i"},
                    "m": {"$mod": [2, 0]},
                    "e": {"$elemMatch": {"a": 1}},
                    "d.x": 1,
                }
            },
        ],
    ],
)
def test_the_redact_safe_part_of_a_match_is_copied(
    rust_mongo: Any, mongod_rs: Any, pipeline: list
) -> None:
    """The part of a `$match` behind a `$redact` that mongod 8.2.11 copies
    ahead of it is its `redactSafePortion` (probed 2026-11-15, RM-580)."""
    assert _stages(rust_mongo, pipeline) == _stages(mongod_rs, pipeline)


def _seed(db: Any) -> None:
    db.c.insert_many([{"_id": n, "x": n} for n in range(300)])


@pytest.mark.backlog_fixed("RM-579")
def test_each_cursor_stage_buffer_takes_an_execution_ticket(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for an aggregate that reads through its cursor
    stage counts an execution admission for each buffer it fills, as each
    takes the global IS apart from the command's own: 50 documents of an
    `$addFields` are two buffers (32 and 64), `Global: {r: 3}` and 4
    admissions. This server's buffers rode the ticket of the locks the
    command held to its end, and counted 2, until RM-579."""
    answers = {}
    for name, client, is_mongod in (("mongod", mongod_rs, True), ("rust", rust_mongo, False)):
        with no_timed_yields(client):
            lines, _ = logged(
                client,
                is_mongod,
                _seed,
                lambda d: d.c.aggregate([{"$addFields": {"q": 1}}], batchSize=50).close(),
            )
        attr = next(a for _, a in lines if next(iter(a.get("command", {})), "") == "aggregate")
        admitted = attr["queues"]["execution"].get("admissions")
        answers[name] = (attr["locks"].get("Global"), admitted)
    assert answers["mongod"] == ({"acquireCount": {"r": 3}}, 4)
    assert answers["rust"] == answers["mongod"]
