"""Gaps found 2026-11-10 while working RM-418 (query settings' `queryFramework`
choosing the engine), each against a one-member mongod 8.2.11 replica set,
which `setQuerySettings` needs."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

DB = "rm418found"


def _clear(client: Any) -> None:
    for e in list(client.admin.aggregate([{"$querySettings": {}}])):
        client.admin.command({"removeQuerySettings": e["queryShapeHash"]})


def _explained(client: Any, cmd: dict, settings: dict | None, index: bool = False) -> dict:
    client.drop_database(DB)
    db = client[DB]
    db.a.insert_many([{"x": i, "y": i % 2} for i in range(5)])
    if index:
        db.a.create_index("x")
    _clear(client)
    if settings is not None:
        client.admin.command({"setQuerySettings": dict(cmd, **{"$db": DB}), "settings": settings})
    try:
        return db.command("explain", cmd, verbosity="queryPlanner")
    finally:
        _clear(client)


def _engine(client: Any, cmd: dict, index: bool = False) -> tuple:
    e = _explained(client, cmd, {"queryFramework": "sbe"}, index)
    qp = e["queryPlanner"] if "queryPlanner" in e else e["stages"][0]["$cursor"]["queryPlanner"]
    return e["explainVersion"], sorted(qp["winningPlan"]), qp["queryHash"]


AGG_AND_DISTINCT = [
    {"aggregate": "a", "pipeline": [{"$match": {"x": 1}}], "cursor": {}},
    {"aggregate": "a", "pipeline": [{"$match": {"x": 1}}, {"$addFields": {"z": 1}}], "cursor": {}},
    {"distinct": "a", "key": "y", "query": {"x": 1}},
]


@pytest.mark.backlog("RM-542")
@pytest.mark.parametrize("cmd", AGG_AND_DISTINCT, ids=["$match", "$match+$addFields", "distinct"])
def test_sbe_framework_pushes_an_aggregate_and_a_distinct(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
    cmd: dict,
) -> None:
    """Under `queryFramework: "sbe"` mongod runs these in SBE."""
    assert _engine(rust_mongo, cmd) == _engine(mongod_rs, cmd)


FINDS = [
    ({"find": "a", "filter": {"x": 1}}, True),
    ({"find": "a", "filter": {"x": {"$gt": 1}}, "sort": {"y": 1}}, False),
    ({"find": "a", "filter": {"x": 1}, "projection": {"y": 1}}, False),
    ({"find": "a", "filter": {"x": {"$in": [1, 2]}}}, False),
]


@pytest.mark.backlog("RM-543")
@pytest.mark.parametrize("cmd,index", FINDS, ids=["index", "sort", "projection", "$in"])
def test_sbe_find_shapes_are_explained_as_sbe(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
    cmd: dict,
    index: bool,
) -> None:
    """mongod explains these `find`s as the slot-based plans it runs."""
    assert _engine(rust_mongo, cmd, index) == _engine(mongod_rs, cmd, index)


def _hinted_key(client: Any) -> str:
    cmd = {"find": "a", "filter": {"x": 1}}
    hints = {"ns": {"db": DB, "coll": "a"}, "allowedIndexes": [{"$natural": 1}]}
    return _explained(client, cmd, {"indexHints": hints})["queryPlanner"]["planCacheKey"]


@pytest.mark.backlog("RM-544")
def test_plan_cache_key_hashes_index_hints(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """A setting's `indexHints` moves mongod's `planCacheKey`."""
    assert _hinted_key(rust_mongo) == _hinted_key(mongod_rs)


def _optimization_time(client: Any) -> bool:
    e = _explained(client, {"find": "a", "filter": {"x": 1}}, None)
    return "optimizationTimeMillis" in e["queryPlanner"]


@pytest.mark.backlog("RM-545")
def test_find_explain_gives_optimization_time(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod's `queryPlanner` has `optimizationTimeMillis`."""
    assert _optimization_time(rust_mongo) == _optimization_time(mongod_rs)
