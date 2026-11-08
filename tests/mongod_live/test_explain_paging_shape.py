"""How `explain` of an aggregate shapes a `$skip` / `$limit` among the
stages that keep the number of documents (RM-308, RM-369, RM-370, RM-371).

mongod optimises a pipeline to a fixed point: a `$match` goes ahead of the
stages that leave the fields it reads (a `$skip` / `$limit` is a wall it
never crosses), and a `$skip` / `$limit` goes ahead of `$project`,
`$unset`, `$addFields` and `$set` as far as the query, which takes it --
never behind an `$unwind`, nor behind a `$match` a `$limit` already keeps
out of the query. Compared are the parsed query, the winning plan's stages
(with `transformBy`, so an int flag is not a boolean one) and the stages
listed after the read. Measured on 8.2.11, 2026-11-08.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.mongod_live.conftest import same

PIPELINES: dict[str, list[dict]] = {
    "project_limit_match": [{"$project": {"x": 1}}, {"$limit": 3}, {"$match": {"x": 1}}],
    "project_skip_match": [{"$project": {"x": 1}}, {"$skip": 1}, {"$match": {"x": 1}}],
    "project_limit": [{"$project": {"x": 1}}, {"$limit": 3}],
    "project_limit_match_limit": [
        {"$project": {"x": 1}},
        {"$limit": 3},
        {"$match": {"x": 1}},
        {"$limit": 2},
    ],
    "project_limit_sort_match": [
        {"$project": {"x": 1}},
        {"$limit": 3},
        {"$sort": {"x": 1}},
        {"$match": {"x": 1}},
    ],
    "project_skip_limit_match": [
        {"$project": {"x": 1}},
        {"$skip": 1},
        {"$limit": 3},
        {"$match": {"x": 1}},
    ],
    "project_limit_split_match": [
        {"$project": {"x": 1}},
        {"$limit": 3},
        {"$match": {"x": 1, "y": 1}},
    ],
    "project_limit_dropped_match": [
        {"$project": {"x": 1}},
        {"$limit": 3},
        {"$match": {"y": 1}},
    ],
    "two_projections_limit_match": [
        {"$project": {"x": 1, "y": 1}},
        {"$limit": 3},
        {"$project": {"x": 1}},
        {"$match": {"x": 1}},
    ],
    "match_project_limit_match": [
        {"$match": {"y": {"$gt": 0}}},
        {"$project": {"x": 1}},
        {"$limit": 3},
        {"$match": {"x": 1}},
    ],
    "rename_limit_match": [{"$project": {"w": "$x"}}, {"$limit": 3}, {"$match": {"w": 1}}],
    "no_id_limit_match": [
        {"$project": {"x": 1, "_id": 0}},
        {"$limit": 3},
        {"$match": {"x": 1}},
    ],
    "unset_limit_match": [{"$unset": "y"}, {"$limit": 3}, {"$match": {"x": 1}}],
    "unset_limit_same_match": [{"$unset": "x"}, {"$limit": 3}, {"$match": {"x": 1}}],
    "limit_project_match": [{"$limit": 3}, {"$project": {"x": 1}}, {"$match": {"x": 1}}],
    "limit_unset_match": [{"$limit": 3}, {"$unset": "y"}, {"$match": {"x": 1}}],
    "limit_add_fields_match": [{"$limit": 3}, {"$addFields": {"w": 1}}, {"$match": {"x": 1}}],
    "limit_sort_match": [{"$limit": 3}, {"$sort": {"x": 1}}, {"$match": {"x": 1}}],
    "limit_project_sort_match": [
        {"$limit": 3},
        {"$project": {"x": 1}},
        {"$sort": {"x": 1}},
        {"$match": {"x": 1}},
    ],
    "limit_match_project_limit": [
        {"$limit": 3},
        {"$match": {"x": 1}},
        {"$project": {"x": 1}},
        {"$limit": 2},
    ],
    "limit_project_limit_match": [
        {"$limit": 3},
        {"$project": {"x": 1}},
        {"$limit": 2},
        {"$match": {"x": 1}},
    ],
    "skip_limit_project_match": [
        {"$skip": 1},
        {"$limit": 3},
        {"$project": {"x": 1}},
        {"$match": {"x": 1}},
    ],
    "match_limit_project_match": [
        {"$match": {"y": 2}},
        {"$limit": 3},
        {"$project": {"x": 1}},
        {"$match": {"x": 1}},
    ],
    "sort_limit_project_match": [
        {"$sort": {"y": 1}},
        {"$limit": 3},
        {"$project": {"x": 1}},
        {"$match": {"x": 1}},
    ],
    "add_fields_project_limit": [
        {"$addFields": {"w": 1}},
        {"$project": {"x": 1}},
        {"$limit": 2},
    ],
    "unwind_project_limit": [{"$unwind": "$t"}, {"$project": {"x": 1}}, {"$limit": 2}],
    "project_limit_add_fields": [
        {"$project": {"x": 1}},
        {"$limit": 3},
        {"$addFields": {"q": 1}},
    ],
}


def _strip(plan: Any) -> Any:
    if not isinstance(plan, dict):
        return plan
    keep = (
        "stage",
        "transformBy",
        "inputStage",
        "limitAmount",
        "skipAmount",
        "filter",
        "sortPattern",
    )
    out = {k: v for k, v in plan.items() if k in keep}
    if "inputStage" in out:
        out["inputStage"] = _strip(out["inputStage"])
    return out


def _shaped(client: Any, pipeline: list[dict]) -> dict:
    c = client["explain_paging_shape"]["c" + uuid.uuid4().hex[:10]]
    c.insert_many([{"_id": i, "x": i, "y": i, "z": i, "t": [i]} for i in range(1, 7)])
    e = c.database.command("aggregate", c.name, pipeline=pipeline, explain=True)
    planner = e.get("queryPlanner") or e["stages"][0]["$cursor"]["queryPlanner"]
    return {
        "parsedQuery": planner.get("parsedQuery"),
        "winningPlan": _strip(planner.get("winningPlan", {})),
        "stages": e.get("stages", [])[1:],
    }


@pytest.mark.parametrize("name", list(PIPELINES))
def test_explain_shapes_paging_as_mongod(rust_mongo: Any, mongod: Any, name: str) -> None:
    same(_shaped(rust_mongo, PIPELINES[name]), _shaped(mongod, PIPELINES[name]))
