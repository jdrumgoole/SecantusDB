"""How a `$match` behind a `$group` moves ahead of it (RM-311).

mongod 8.2 renames a `$match` on a `$group`'s key onto the field the key is
(`DocumentSourceGroupBase::getModifiedPaths`): `_id: "$x"` makes `_id` a
rename of `x`, `_id: {k: "$x"}` makes `_id.k` one, and a dotted `"$a.b"` is
no rename at all. The swap is refused whole when the `$match` has a `$type`
predicate on the key, or an `$exists` on a key of one field
(`groupMatchSwapVerified`). What moves is split as for every stage: the
children of an `$and` one by one, the wholly movable children of a `$nor`,
an `$or` or `$expr` only whole. The same split, renames included, moves a
`$match` ahead of a `$project` and an `$addFields`. Compared are the parsed
query, the stages after the read and the documents. A `$group` listed
after the read is left out: mongod runs it in its slot-based engine for
more shapes than this server explains that way (RM-071). The winning plan
is left out too: its projection's name is RM-445. The clauses of a
`$match` left behind are compared in a fixed order, as RM-444 owns the
order mongod lists them in. Measured on 8.2.11, 2026-11-09.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest
from bson import json_util

from tests.mongod_live.conftest import same

_G = {"$group": {"_id": "$x", "f": {"$first": "$y"}}}

PIPELINES: dict[str, list[dict]] = {
    "gt": [_G, {"$match": {"_id": {"$gt": 1}}}],
    "null": [_G, {"$match": {"_id": None}}],
    "exists_false": [_G, {"$match": {"_id": {"$exists": False}}}],
    "exists_true": [_G, {"$match": {"_id": {"$exists": True}}}],
    "type": [_G, {"$match": {"_id": {"$type": "array"}}}],
    "type_beside_a_movable_clause": [_G, {"$match": {"_id": {"$type": "number"}, "f": 2}}],
    "dotted_key": [{"$group": {"_id": "$a.b"}}, {"$match": {"_id": 2}}],
    "root_key": [{"$group": {"_id": "$$ROOT.x"}}, {"$match": {"_id": 2}}],
    "current_key": [{"$group": {"_id": "$$CURRENT.x"}}, {"$match": {"_id": 2}}],
    "computed_key": [{"$group": {"_id": {"$add": ["$y", 1]}}}, {"$match": {"_id": 2}}],
    "document_key": [
        {"$group": {"_id": {"k": "$x"}, "f": {"$first": "$y"}}},
        {"$match": {"_id.k": {"$gt": 1}}},
    ],
    "document_key_whole": [{"$group": {"_id": {"k": "$x"}}}, {"$match": {"_id": {"k": 2}}}],
    "document_key_exists": [
        {"$group": {"_id": {"k": "$x"}}},
        {"$match": {"_id.k": {"$exists": True}}},
    ],
    "compound_key_exists": [
        {"$group": {"_id": {"k": "$x", "c": "$y"}}},
        {"$match": {"_id.k": {"$exists": True}, "_id.c": 2}},
    ],
    "compound_key_partly": [
        {"$group": {"_id": {"k": "$x", "c": {"$add": ["$y", 1]}}}},
        {"$match": {"_id.k": 2, "_id.c": 3}},
    ],
    "subpath": [_G, {"$match": {"_id.z": 2}}],
    "accumulator": [_G, {"$match": {"f": {"$gt": 1}}}],
    "and": [_G, {"$match": {"$and": [{"_id": 2}, {"f": 2}]}}],
    "or": [_G, {"$match": {"$or": [{"_id": 2}, {"_id": 3}]}}],
    "or_with_accumulator": [_G, {"$match": {"$or": [{"_id": 2}, {"f": 3}]}}],
    "nor": [_G, {"$match": {"$nor": [{"_id": 2}, {"f": 3}]}}],
    "expr": [_G, {"$match": {"$expr": {"$eq": ["$_id", 2]}}}],
    "elem_match": [_G, {"$match": {"_id": {"$elemMatch": {"$gt": 3}}}}],
    "mixed": [_G, {"$match": {"_id": {"$gte": 2}, "f": {"$lt": 5}}}],
    "after_sort": [_G, {"$sort": {"_id": 1}}, {"$match": {"_id": 2}}],
    "after_add_fields": [_G, {"$addFields": {"q": 1}}, {"$match": {"_id": 2}}],
    "two_groups": [
        {"$group": {"_id": "$x", "y": {"$first": "$y"}}},
        {"$group": {"_id": "$_id"}},
        {"$match": {"_id": 2}},
    ],
    "match_group_match": [{"$match": {"y": {"$gt": 0}}}, _G, {"$match": {"_id": 2}}],
    "project_rename_or": [
        {"$project": {"w": "$x"}},
        {"$match": {"$or": [{"w": 2}, {"w": 3}]}},
    ],
    "project_rename_expr": [{"$project": {"w": "$x"}}, {"$match": {"$expr": {"$eq": ["$w", 2]}}}],
    "add_fields_and": [{"$addFields": {"z": 1}}, {"$match": {"$and": [{"y": 2}, {"z": 1}]}}],
    "add_fields_nor": [{"$addFields": {"z": 1}}, {"$match": {"$nor": [{"y": 2}, {"z": 3}]}}],
}


def _clauses_sorted(stage: dict) -> dict:
    """A listed `$match` with its `$and` / `$or` children in a fixed order."""
    m = stage.get("$match")
    if not isinstance(m, dict) or len(m) != 1:
        return stage
    ((op, children),) = m.items()
    if op not in ("$and", "$or") or not isinstance(children, list):
        return stage
    return {"$match": {op: sorted(children, key=lambda c: json_util.dumps(c, sort_keys=True))}}


def _shaped(client: Any, pipeline: list[dict]) -> dict:
    c = client["group_match_pushdown"]["c" + uuid.uuid4().hex[:10]]
    c.insert_many(
        [
            {"_id": 1, "x": 1, "y": 1},
            {"_id": 2, "x": 2, "y": 2},
            {"_id": 3, "y": 3},
            {"_id": 4, "x": None, "y": 4},
            {"_id": 5, "x": [2, 5], "y": 5},
            {"_id": 6, "a": [{"b": 1}, {"b": 2}], "y": 6},
            {"_id": 7, "a": {"b": 3}, "x": 3, "y": 7},
        ]
    )
    e = c.database.command("aggregate", c.name, pipeline=pipeline, explain=True)
    planner = e.get("queryPlanner") or e["stages"][0]["$cursor"]["queryPlanner"]
    docs = sorted(json_util.dumps(d, sort_keys=True) for d in c.aggregate(pipeline))
    return {
        "parsedQuery": planner.get("parsedQuery"),
        "stages": [_clauses_sorted(s) for s in e.get("stages", [])[1:] if "$group" not in s],
        "docs": docs,
    }


@pytest.mark.parametrize("name", list(PIPELINES))
def test_a_match_moves_ahead_of_a_group_as_on_mongod(
    rust_mongo: Any, mongod: Any, name: str
) -> None:
    same(_shaped(rust_mongo, PIPELINES[name]), _shaped(mongod, PIPELINES[name]))
