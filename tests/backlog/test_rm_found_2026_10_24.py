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
from bson import json_util

from tests.backlog.conftest import mongo_answer

#: Canonical extended JSON tells an int flag from a boolean one, which a plain
#: Python comparison does not (`True == 1`).
_CANONICAL = json_util.JSONOptions(json_mode=json_util.JSONMode.CANONICAL)


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


@pytest.mark.backlog_fixed("RM-308")
def test_explain_lists_the_projection_a_limit_moved_past(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains `[$project, $limit, $match]` as a read with the
    limit, then the `$match` and the `$project`; this server listed the
    `$match` alone, having taken the projection into the query."""
    got, want = _explained(rust_mongo, _LEFT_BEHIND), _explained(mongod, _LEFT_BEHIND)
    assert [list(s) for s in want["project_limit_match"][1]] == [["$match"], ["$project"]]
    assert got == want


#: Pipelines whose explain shaping was probed on 2026-11-06, while RM-308 was
#: being worked: a `$project` that the query absorbs, a `$skip` in the same
#: place, an exclusion projection, and a `$limit` sitting between a reshape
#: and a `$match`.
_SHAPING = {
    "project_limit_match": [{"$project": {"x": 1}}, {"$limit": 3}, {"$match": {"x": 1}}],
    "project_skip_match": [{"$project": {"x": 1}}, {"$skip": 1}, {"$match": {"x": 1}}],
    "exclusion_project_limit_match": [
        {"$project": {"y": 0}},
        {"$limit": 3},
        {"$match": {"x": 1}},
    ],
    "addfields_limit_match": [{"$addFields": {"w": 1}}, {"$limit": 3}, {"$match": {"x": 1}}],
}


def _planned(client: Any, pipeline: list) -> Any:
    """The winning plan of `pipeline`'s read, with only the fields that say
    which stages the query took."""
    c = _coll(client)
    c.insert_many([{"_id": i, "x": i, "y": i, "z": i} for i in range(1, 7)])
    e = c.database.command("aggregate", c.name, pipeline=pipeline, explain=True)
    planner = e.get("queryPlanner") or e["stages"][0]["$cursor"]["queryPlanner"]

    def strip(plan: Any) -> Any:
        if not isinstance(plan, dict):
            return plan
        keep = {
            k: v
            for k, v in plan.items()
            if k in ("stage", "transformBy", "inputStage", "limitAmount", "skipAmount")
        }
        if "inputStage" in keep:
            keep["inputStage"] = strip(keep["inputStage"])
        return keep

    return strip(planner.get("winningPlan", {}))


@pytest.mark.backlog_fixed("RM-369")
@pytest.mark.parametrize(
    "name", ["project_limit_match", "project_skip_match"], ids=["limit", "skip"]
)
def test_an_inclusion_projection_keeps_its_int_flags_in_the_plan(
    rust_mongo: Any, mongod: Any, name: str
) -> None:
    """mongod 8.2.11 plans `{$project: {x: 1}}` behind a `$limit` / `$skip`
    and a `$match` with the `transformBy` `{_id: 1, x: 1}`, where this server
    printed booleans, `{_id: true, x: true}` (measured 2026-11-06). The int
    flags are not the user's: mongod keeps the `$project` in the pipeline and
    projects the fields the `$match` and it read, which is what this server
    now does too (RM-308)."""
    # Canonical extended JSON, because Python's `True == 1`: a plain dict
    # comparison calls an int flag and a boolean one equal, which is the whole
    # difference here.
    got = json_util.dumps(_planned(rust_mongo, _SHAPING[name]), json_options=_CANONICAL)
    want = json_util.dumps(_planned(mongod, _SHAPING[name]), json_options=_CANONICAL)
    assert got == want


@pytest.mark.backlog_fixed("RM-370")
def test_an_exclusion_projection_is_not_pushed_into_the_plan(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 does not push an EXCLUSION projection into the read: the
    plan of `[{$project: {y: 0}}, {$limit: 3}, {$match: ...}]` is
    `LIMIT -> COLLSCAN` with no projection stage at all. This server emitted a
    `PROJECTION_SIMPLE` with `{y: false, _id: true}` (measured 2026-11-06)."""
    pipeline = _SHAPING["exclusion_project_limit_match"]
    got, want = _planned(rust_mongo, pipeline), _planned(mongod, pipeline)
    assert got == want


@pytest.mark.backlog_fixed("RM-371")
def test_a_limit_between_a_reshape_and_a_match_is_still_taken(rust_mongo: Any, mongod: Any) -> None:
    """A `$limit` between an `$addFields` and a `$match` does not stop
    mongod 8.2.11 taking it: `[{$addFields}, {$limit: 3}, {$match}]` reads
    `LIMIT -> COLLSCAN` and lists `[$match, $addFields]`, the `$match` moved
    ahead of the stage it reads nothing of. This server took nothing and
    listed all three stages in their written order (measured 2026-11-06)."""
    pipeline = _SHAPING["addfields_limit_match"]
    assert _planned(rust_mongo, pipeline) == _planned(mongod, pipeline)
    assert _explained(rust_mongo, {"p": pipeline}) == _explained(mongod, {"p": pipeline})
