"""Divergences found while shaping the rest of mongod's query shapes
(RM-358, 2026-11-09), each measured against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest
from bson.son import SON
from pymongo.errors import OperationFailure


def _explain(client: Any, command: SON) -> dict:
    db = client["rm_found_rm358_explain"]
    db.c.insert_one({"x": 1})
    try:
        reply = db.command("explain", command, verbosity="queryPlanner")
    except OperationFailure as e:
        return {"error": e.code}
    finally:
        client.drop_database("rm_found_rm358_explain")
    return {k: reply.get(k) for k in ("stages", "queryShapeHash")}


@pytest.mark.backlog_fixed("RM-466")
def test_an_explain_of_plan_cache_stats_names_its_stage(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains `aggregate` of `[{$planCacheStats: {}}]`: its
    `stages` hold the stage with its default (`allHosts: false`), then its
    `queryShapeHash`. This server's explain failed: it ran the pipeline
    against a namespace of its own (`explain.dry.run`), which does not
    exist."""
    command = SON([("aggregate", "c"), ("pipeline", [{"$planCacheStats": {}}]), ("cursor", {})])
    answers = {"mongod": _explain(mongod, command), "rust": _explain(rust_mongo, command)}
    assert answers["mongod"].get("stages") == [{"$planCacheStats": {"allHosts": False}}]
    assert answers["rust"] == answers["mongod"]


# ------------------------------------------------- found 2026-11-12 --


@pytest.mark.backlog("RM-552")
def test_an_explain_of_geo_near_runs(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains `aggregate` of `[{$geoNear: {near: [0, 0]}}]`
    over a collection with a `2d` index, with or without `distanceField`,
    and prints its `queryShapeHash`. This server's explain fails with 26
    (`$geoNear requires a geo index to run, but <db>.explain.dry.run does
    not exist`): it runs the pipeline against a namespace of its own, as
    RM-466 does for `$planCacheStats` (found 2026-11-12, RM-358)."""

    def explain(client: Any, spec: dict) -> Any:
        db = client["rm_found_rm358_geo"]
        db.geo.insert_one({"loc": [0, 0]})
        db.geo.create_index([("loc", "2d")])
        command = SON([("aggregate", "geo"), ("pipeline", [{"$geoNear": spec}]), ("cursor", {})])
        try:
            return db.command("explain", command, verbosity="queryPlanner").get("queryShapeHash")
        except OperationFailure as e:
            return {"error": e.code}
        finally:
            client.drop_database("rm_found_rm358_geo")

    specs = [{"near": [0, 0], "distanceField": "d"}, {"near": [0, 0]}]
    answers = {
        s: [explain(c, spec) for spec in specs]
        for s, c in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert all(isinstance(h, str) for h in answers["mongod"]), answers["mongod"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-553")
def test_score_fusion_combines_by_avg(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$scoreFusion` takes `combination.method: "avg"`
    (its IDL enum, `kAvg: "avg"`) and refuses `"average"` with 2
    (`Enumeration value 'average' for field '$scoreFusion.combination.method'
    is not a valid value.`). This server takes `"average"` and refuses
    `"avg"` (found 2026-11-12, RM-358)."""

    def run(client: Any, method: str) -> Any:
        db = client["rm_found_rm358_avg"]
        db.c.insert_many([{"_id": n, "x": n} for n in range(3)])
        stage = {
            "$scoreFusion": {
                "input": {
                    "pipelines": {"a": [{"$score": {"score": "$x"}}]},
                    "normalization": "none",
                },
                "combination": {"method": method},
            }
        }
        try:
            return [d["_id"] for d in db.c.aggregate([stage])]
        except OperationFailure as e:
            return {"error": e.code}
        finally:
            client.drop_database("rm_found_rm358_avg")

    answers = {
        s: {m: run(c, m) for m in ("avg", "average")}
        for s, c in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == {"avg": [2, 1, 0], "average": {"error": 2}}
    assert answers["rust"] == answers["mongod"]


_P = {"$planCacheStats": {}}


def _stages(client: Any, coll: str, pipeline: list, verbosity: str) -> Any:
    db = client["rm_found_rm466_explain"]
    db.c.insert_one({"x": 1})
    try:
        command = SON([("aggregate", coll), ("pipeline", pipeline), ("cursor", {})])
        reply = db.command("explain", command, verbosity=verbosity)
    except OperationFailure as e:
        return {"error": e.code, "errmsg": e.details.get("errmsg")}
    finally:
        client.drop_database("rm_found_rm466_explain")
    return reply.get("stages")


@pytest.mark.backlog_fixed("RM-466")
@pytest.mark.parametrize(
    ("coll", "pipeline", "verbosity"),
    [
        # A `$match` right after the stage is absorbed into it, as written.
        ("c", [_P, {"$match": {"a": {"$in": [1]}}}], "queryPlanner"),
        # ... and one that reaches it past a `$project` or a `$sort`.
        ("c", [_P, {"$project": {"a": 1}}, {"$match": {"a": 1}}], "queryPlanner"),
        # One rewritten on the way is absorbed parsed.
        ("c", [_P, {"$project": {"c": "$a"}}, {"$match": {"c": 1}}], "queryPlanner"),
        ("c", [_P, {"$addFields": {"c": 1}}, {"$match": {"a": 1, "c": 1}}], "queryPlanner"),
        # A second `$match` right after the first stays where it is.
        ("c", [_P, {"$match": {"a": 1}}, {"$match": {"b": 2}}], "queryPlanner"),
        # One that a later stage swaps ahead replaces the one absorbed.
        (
            "c",
            [_P, {"$match": {"a": 1}}, {"$sort": {"a": 1}}, {"$match": {"b": 1}}],
            "queryPlanner",
        ),
        (
            "c",
            [_P, {"$sort": {"a": 1}}, {"$match": {"a": 1}}, {"$match": {"b": 1}}],
            "queryPlanner",
        ),
        ("c", [_P, {"$match": {"a": 1}}, {"$limit": 1}, {"$match": {"b": 1}}], "queryPlanner"),
        # Listed, a missing collection is no matter; run, it is.
        ("missing", [_P, {"$match": {"a": 1}}], "queryPlanner"),
        ("missing", [_P], "executionStats"),
        ("c", [_P, {"$match": {"a": 1}}, {"$limit": 2}], "executionStats"),
    ],
)
def test_an_explain_of_plan_cache_stats_absorbs_matches_as_mongod_does(
    rust_mongo: Any, mongod: Any, coll: str, pipeline: list, verbosity: str
) -> None:
    """mongod 8.2.11's optimizer folds a `$match` into `$planCacheStats`
    (`match`, ahead of `allHosts`) and replaces it with each later one a
    stage swaps ahead; `executionStats` of a missing collection fails with
    50933 and no executor's prefix. The fix for RM-466 lists them so."""
    answers = {
        "mongod": _stages(mongod, coll, pipeline, verbosity),
        "rust": _stages(rust_mongo, coll, pipeline, verbosity),
    }
    assert answers["rust"] == answers["mongod"]
