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


@pytest.mark.backlog("RM-466")
def test_an_explain_of_plan_cache_stats_names_its_stage(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains `aggregate` of `[{$planCacheStats: {}}]`: its
    `stages` hold the stage with its default (`allHosts: false`), then its
    `queryShapeHash`. This server's explain fails: it runs the pipeline
    against a namespace of its own (`explain.dry.run`), which does not
    exist."""
    command = SON([("aggregate", "c"), ("pipeline", [{"$planCacheStats": {}}]), ("cursor", {})])
    answers = {"mongod": _explain(mongod, command), "rust": _explain(rust_mongo, command)}
    assert answers["mongod"].get("stages") == [{"$planCacheStats": {"allHosts": False}}]
    assert answers["rust"] == answers["mongod"]
