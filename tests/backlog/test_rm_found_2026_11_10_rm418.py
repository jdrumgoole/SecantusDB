"""Gaps found 2026-11-10 while working RM-418 (query settings' `queryFramework`
choosing the engine), each against a one-member mongod 8.2.11 replica set,
which `setQuerySettings` needs."""

from __future__ import annotations

import re
from typing import Any

import pytest
from bson import Int64

from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

DB = "rm418found"


def _clear(client: Any) -> None:
    for e in list(client.admin.aggregate([{"$querySettings": {}}])):
        client.admin.command({"removeQuerySettings": e["queryShapeHash"]})


def _explained(
    client: Any,
    cmd: dict,
    settings: dict | None,
    index: bool = False,
    verbosity: str = "queryPlanner",
) -> dict:
    client.drop_database(DB)
    db = client[DB]
    db.a.insert_many([{"x": i, "y": i % 2} for i in range(5)])
    if index:
        db.a.create_index("x")
    _clear(client)
    if settings is not None:
        client.admin.command({"setQuerySettings": dict(cmd, **{"$db": DB}), "settings": settings})
    try:
        return db.command("explain", cmd, verbosity=verbosity)
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


@pytest.mark.backlog_fixed("RM-542")
@pytest.mark.parametrize("cmd", AGG_AND_DISTINCT, ids=["$match", "$match+$addFields", "distinct"])
def test_sbe_framework_pushes_an_aggregate_and_a_distinct(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
    cmd: dict,
) -> None:
    """Under `queryFramework: "sbe"` mongod runs these in SBE; this server
    pushed them as without the setting until RM-542 was fixed."""
    assert _engine(rust_mongo, cmd) == _engine(mongod_rs, cmd)


FINDS = [
    ({"find": "a", "filter": {"x": 1}}, True),
    ({"find": "a", "filter": {"x": {"$gt": 1}}, "sort": {"y": 1}}, False),
]


@pytest.mark.backlog("RM-543")
@pytest.mark.parametrize("cmd,index", FINDS, ids=["index", "sort"])
def test_sbe_find_shapes_are_explained_as_sbe(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
    cmd: dict,
    index: bool,
) -> None:
    """mongod explains these `find`s as the slot-based plans it runs."""
    assert _engine(rust_mongo, cmd, index) == _engine(mongod_rs, cmd, index)


def _find(**fields: Any) -> dict:
    return {"find": "a", **fields}


BUILT_FINDS = [
    _find(filter={"x": {"$in": [1, 2]}}),
    _find(filter={"x": {"$in": [2, 1, 1]}}),
    _find(filter={"x": {"$in": [2.0, 1.5, Int64(3), -0.0, 1e20, "a b", True]}}),
    _find(filter={"x": {"$in": [1, 2]}, "y": {"$gt": 0}}),
    _find(filter={"x": {"$in": [1, 2]}, "y": {"$in": [3, 4]}}),
    _find(filter={"d.e": {"$in": [1, 2]}}),
    _find(filter={"x": {"$in": [1, 2]}}, skip=1, limit=2),
    _find(filter={"x": 1}, projection={"y": 1}),
    _find(filter={"x": 1}, projection={"_id": 0, "y": 1}),
    _find(filter={"x": 1}, projection={"_id": 0, "x": 1}),
    _find(filter={"x": 1}, projection={"_id": 1, "z": 1, "y": 1}),
    _find(filter={"x": 1}, projection={"_id": 0, "z": 1, "y": 1}),
    _find(filter={"x": 1}, projection={"z": 0, "y": 0}),
    _find(filter={"x": 1}, projection={"_id": 0}),
    _find(filter={"x": 1}, projection={"_id": 1}),
    _find(filter={}, projection={"y": 1}),
    _find(filter={"x": 1}, projection={"y": 1}, limit=2),
    _find(filter={"x": 1}, projection={"y": 1}, skip=1),
    _find(filter={"x": {"$in": [1, 2]}}, projection={"y": 1}),
]


def _built(client: Any, cmd: dict) -> tuple:
    """The plan and its run, the collection's UUID taken out."""
    e = _explained(client, cmd, {"queryFramework": "sbe"}, verbosity="executionStats")
    wp = e["queryPlanner"]["winningPlan"]
    sbp = {k: re.sub(r'@"[0-9a-f-]{36}"', '@"uuid"', v) for k, v in wp["slotBasedPlan"].items()}
    stats = e["executionStats"]
    for k in ("executionTimeMillis",):
        stats.pop(k, None)
    return e["explainVersion"], wp["queryPlan"], sbp, _untimed(stats)


def _untimed(d: Any) -> Any:
    if isinstance(d, dict):
        return {k: _untimed(v) for k, v in d.items() if k != "executionTimeMillisEstimate"}
    return d


@pytest.mark.backlog_fixed("RM-543")
@pytest.mark.parametrize(
    "cmd", BUILT_FINDS, ids=lambda c: str({k: v for k, v in c.items() if k != "find"})
)
def test_sbe_find_in_and_projection_are_mongods(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
    cmd: dict,
) -> None:
    """An `$in` and a simple projection under `queryFramework: "sbe"` were
    explained in the classic shape; they now print and run as mongod's
    slot programs (`isMember` over an `InList`, `makeBsonObj`, `newBsonObj`)."""
    assert _built(rust_mongo, cmd) == _built(mongod_rs, cmd)


def _hinted_key(client: Any, hints: Any = None) -> str:
    cmd = {"find": "a", "filter": {"x": 1}}
    if hints is None:
        hints = {"ns": {"db": DB, "coll": "a"}, "allowedIndexes": [{"$natural": 1}]}
    e = _explained(client, cmd, {"indexHints": hints}, index=True)
    return e["queryPlanner"]["planCacheKey"]


@pytest.mark.backlog_fixed("RM-544")
def test_plan_cache_key_hashes_index_hints(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """A setting's `indexHints` moves mongod's `planCacheKey`; this server
    hashed the hints as unset."""
    assert _hinted_key(rust_mongo) == _hinted_key(mongod_rs)


def _spec(*allowed: Any, coll: str = "a") -> dict:
    return {"ns": {"db": DB, "coll": coll}, "allowedIndexes": list(allowed)}


HINT_SHAPES = [
    _spec({"$natural": -1}),
    _spec("x_1"),
    _spec({"x": 1}),
    _spec({"x": -1.5, "y": "hashed"}),
    _spec({"x": 1}, "x_1", {"$natural": 1}),
    [_spec("x_1"), _spec({"y": 1}, coll="b")],
    _spec({"a.b": Int64(1)}, "a_rather_long_index_name_over_sixteen_bytes"),
]


@pytest.mark.backlog_fixed("RM-544")
@pytest.mark.parametrize("hints", HINT_SHAPES, ids=str)
def test_plan_cache_key_hashes_each_hint_shape(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
    hints: Any,
) -> None:
    """Names, key patterns, `$natural` and several specs hash as mongod's
    `query_settings_hash.cpp` hashes them."""
    assert _hinted_key(rust_mongo, hints) == _hinted_key(mongod_rs, hints)


def _optimization_time(client: Any) -> bool:
    e = _explained(client, {"find": "a", "filter": {"x": 1}}, None)
    return "optimizationTimeMillis" in e["queryPlanner"]


@pytest.mark.backlog_fixed("RM-545")
def test_find_explain_gives_optimization_time(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """A `find` explain lacked mongod's `optimizationTimeMillis`."""
    assert _optimization_time(rust_mongo) == _optimization_time(mongod_rs)
