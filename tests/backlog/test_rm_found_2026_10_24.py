"""Gap tests for the divergences found on 2026-10-24 while streaming
`aggregate` cursors (RM-080): the wording of an error a streamed `getMore`
meets, and how `explain` moves a `$match` ahead of the stages before it.

The reference is mongod 8.2.11, asked live (`mongod`); the Rust server is
`secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _coll(client: Any) -> Any:
    return client["rm_found_1024"]["c" + uuid.uuid4().hex[:10]]


def _get_more_errors(client: Any) -> list:
    """What a `getMore` a document at a time meets in the third document of
    a `find` and of two `aggregate`s: the second document, the error, and
    then no cursor."""
    out = []
    convert = {"$toInt": "$s"}
    for command, spec in (
        ("find", {"filter": {"$expr": {"$gt": [convert, 0]}}, "batchSize": 1}),
        (
            "aggregate",
            {
                "pipeline": [{"$match": {"$expr": {"$gt": [convert, 0]}}}],
                "cursor": {"batchSize": 1},
            },
        ),
        ("aggregate", {"pipeline": [{"$addFields": {"n": convert}}], "cursor": {"batchSize": 1}}),
    ):
        c = _coll(client)
        c.insert_many([{"_id": i, "s": "abc" if i == 3 else str(i)} for i in range(1, 10)])
        cursor = c.database.command(command, c.name, **spec)["cursor"]["id"]

        def more(cursor: int = cursor, c: Any = c) -> Any:
            reply = c.database.command("getMore", cursor, collection=c.name, batchSize=1)
            return [d["_id"] for d in reply["cursor"]["nextBatch"]]

        out.append((mongo_answer(more), mongo_answer(more), mongo_answer(more)[:2]))
    return out


@pytest.mark.backlog_fixed("RM-306")
def test_a_streamed_get_more_names_itself_in_an_executor_error(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 words an error met reading a cursor's later batch as
    the `getMore`'s (`Executor error during getMore :: caused by :: ...`),
    not the command's that opened it; the cursor is then gone (43)."""
    got, want = _get_more_errors(rust_mongo), _get_more_errors(mongod)
    for second, error, after in want:
        assert second == ("ok", [2])
        assert error[1] == 241
        assert error[2].startswith("Executor error during getMore :: caused by :: ")
        assert after == ("err", 43)
    assert got == want


_HOISTED = {
    "inclusion": [{"$project": {"x": 1}}, {"$match": {"x": {"$gte": 0}}}],
    "inclusion_split": [{"$project": {"x": 1}}, {"$match": {"x": 1, "z": 1}}],
    "inclusion_id": [{"$project": {"x": 1}}, {"$match": {"_id": 1}}],
    "inclusion_without_id": [{"$project": {"x": 1, "_id": 0}}, {"$match": {"_id": 1}}],
    "exclusion": [{"$project": {"x": 0}}, {"$match": {"y": 1}}],
    "exclusion_same": [{"$project": {"x": 0}}, {"$match": {"x": 1}}],
    "rename": [{"$project": {"w": "$x"}}, {"$match": {"w": 1}}],
    "computed": [{"$project": {"x": 1, "w": {"$add": ["$x", 1]}}}, {"$match": {"x": 1}}],
    "unset": [{"$unset": "x"}, {"$match": {"y": 1}}],
    "unset_same": [{"$unset": "x"}, {"$match": {"x": 1}}],
    "dotted": [{"$project": {"a.b": 1}}, {"$match": {"a.b": 1}}],
    "dotted_parent": [{"$project": {"a.b": 1}}, {"$match": {"a": {"b": 1}}}],
    "expr": [{"$project": {"x": 1}}, {"$match": {"$expr": {"$eq": ["$x", 1]}}}],
    "two_projections": [
        {"$project": {"x": 1, "y": 1}},
        {"$project": {"x": 1}},
        {"$match": {"x": 1}},
    ],
    "past_a_sort": [{"$project": {"x": 1}}, {"$sort": {"x": 1}}, {"$match": {"x": 1}}],
    "past_add_fields": [{"$addFields": {"q": 1}}, {"$project": {"x": 1}}, {"$match": {"x": 1}}],
}


def _explained(client: Any, pipelines: dict) -> dict:
    """Each pipeline's query filter and the stages left after the read."""
    c = _coll(client)
    c.insert_many([{"_id": i, "x": i, "y": i, "z": i, "a": {"b": i}} for i in range(1, 7)])
    out = {}
    for name, pipeline in pipelines.items():
        e = c.database.command("aggregate", c.name, pipeline=pipeline, explain=True)
        planner = e.get("queryPlanner") or e["stages"][0]["$cursor"]["queryPlanner"]
        out[name] = (planner.get("parsedQuery"), e.get("stages", [])[1:])
    return out


@pytest.mark.backlog_fixed("RM-307")
def test_explain_moves_a_match_ahead_of_a_projection_that_keeps_its_fields(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 moves a `$match` clause ahead of a `$project` or
    `$unset` that leaves every field it reads (onto the field a rename
    names), and ahead of a `$sort`, so the query reads it; a clause on a
    field the stage drops or computes stays behind."""
    got, want = _explained(rust_mongo, _HOISTED), _explained(mongod, _HOISTED)
    assert want["inclusion"] == ({"x": {"$gte": 0}}, [])
    assert want["rename"] == ({"x": {"$eq": 1}}, [])
    assert want["exclusion_same"][0] == {}
    assert got == want


_LEFT_BEHIND = {
    "project_limit_match": [{"$project": {"x": 1}}, {"$limit": 3}, {"$match": {"x": 1}}],
}


@pytest.mark.backlog("RM-308")
def test_explain_lists_the_projection_a_limit_moved_past(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains `[$project, $limit, $match]` as a read with the
    limit, then the `$match` and the `$project`; this server lists the
    `$match` alone."""
    got, want = _explained(rust_mongo, _LEFT_BEHIND), _explained(mongod, _LEFT_BEHIND)
    assert [list(s) for s in want["project_limit_match"][1]] == [["$match"], ["$project"]]
    assert got == want
