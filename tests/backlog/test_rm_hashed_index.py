"""Gap tests for hashed indexes on the Rust MongoDB server, against mongod
8.2.11 (measured 2026-11-20).

A hashed index works on a single-node mongod: it is not a sharding-only
feature. The Rust server refuses every one with 67 CannotCreateIndex ("not
supported by the Rust storage engine yet").
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer, mongod_rs, rust_mongo  # noqa: F401  (fixtures)


def _created(client: Any, coll: str, spec: dict) -> tuple:
    """`createIndexes` as `("ok",)` or `("err", code, errmsg)`."""
    db = client.rm_hashed
    ans = mongo_answer(lambda: db.command("createIndexes", coll, indexes=[spec]))
    return ("ok",) if ans[0] == "ok" else ans


def _plan_shape(plan: dict) -> list:
    """The winning plan's stages, outermost first, with each index scan's key
    pattern and bounds."""
    out = []
    while plan:
        step = [plan["stage"]]
        if plan["stage"] == "IXSCAN":
            step += [dict(plan["keyPattern"]), dict(plan["indexBounds"])]
        out.append(step)
        plan = plan.get("inputStage")
    return out


def _hashed_basics(client: Any) -> tuple:
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    made = _created(client, "c", {"key": {"a": "hashed"}, "name": "a_hashed"})
    listed = [dict(i["key"]) for i in db.c.list_indexes()]
    db.c.insert_many([{"a": i} for i in range(20)])
    eq = _plan_shape(db.c.find({"a": 3}).explain()["queryPlanner"]["winningPlan"])
    rng = _plan_shape(db.c.find({"a": {"$gt": 3}}).explain()["queryPlanner"]["winningPlan"])
    found = list(db.c.find({"a": 3}, {"_id": 0}))
    client.drop_database("rm_hashed")
    return made, listed, eq, rng, found


@pytest.mark.backlog("RM-732")
def test_a_hashed_index_is_built_and_serves_equality(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11: `{a: "hashed"}` is created and listed; `{a: 3}` is a
    FETCH over an IXSCAN of it with the point bounds
    `[-1529429926180645992, -1529429926180645992]` (mongod's hash of 3); a
    range query cannot use it and is a COLLSCAN."""
    theirs = _hashed_basics(mongod_rs)
    assert theirs[0] == ("ok",), theirs
    assert _hashed_basics(rust_mongo) == theirs


def _refusals(client: Any) -> list:
    client.drop_database("rm_hashed")
    db = client.rm_hashed
    out = [
        _created(client, "u", {"key": {"b": "hashed"}, "name": "b_h", "unique": True}),
        _created(client, "two", {"key": {"x": "hashed", "y": "hashed"}, "name": "xy"}),
    ]
    _created(client, "arr", {"key": {"a": "hashed"}, "name": "a_hashed"})
    reply = db.command("insert", "arr", documents=[{"a": [1, 2]}])
    out.append([(e["code"], e["errmsg"]) for e in reply.get("writeErrors", [])])
    client.drop_database("rm_hashed")
    return out


@pytest.mark.backlog("RM-732")
def test_hashed_index_refusals_are_mongods(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses a unique hashed index with 16764 ("Currently
    hashed indexes cannot guarantee uniqueness. Use a regular index."), two
    hashed fields with 31303 ("A maximum of one index field is allowed to be
    hashed ..."), and an array value under a hashed index with the write
    error 16766 ("Error: hashed indexes do not currently support array
    values. Found array at path: a")."""
    theirs = _refusals(mongod_rs)
    assert [r[1] for r in theirs[:2]] == [16764, 31303], theirs
    assert [c for c, _ in theirs[2]] == [16766], theirs
    assert _refusals(rust_mongo) == theirs


@pytest.mark.backlog("RM-732")
def test_a_compound_index_with_one_hashed_field_is_built(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 accepts `{x: 1, y: "hashed"}`: one hashed field may sit
    in a compound index."""

    def made(client: Any) -> tuple:
        client.drop_database("rm_hashed")
        ans = _created(client, "cmp", {"key": {"x": 1, "y": "hashed"}, "name": "x1_yh"})
        client.drop_database("rm_hashed")
        return ans

    theirs = made(mongod_rs)
    assert theirs == ("ok",), theirs
    assert made(rust_mongo) == theirs
