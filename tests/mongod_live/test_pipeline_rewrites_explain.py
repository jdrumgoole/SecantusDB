"""The pipeline rewrites mongod's optimizer makes, as `explain` of an
aggregate over a collection lists them (RM-538).

mongod 8.2 drops a `$replaceRoot` / `$replaceWith` of the document itself
(`$$ROOT`, `$$CURRENT`); a `$sort` takes the `$limit` behind it as its own
`limit`, over the stages that keep the count (`$project`, `$addFields`,
`$replaceRoot`, `$lookup`) and over a `$skip`, whose amount the limit then
counts (`DocumentSourceSort::doOptimizeAt`), and is listed as one `$sort`
with that `limit`; a `$match` on a field an `$addFields` / `$set` sets to a
field path of one component moves ahead of it onto that field, in an `$or`
and an `$expr` too; and `$sampleRate` is parsed into `{$expr: {$lt:
[{$rand: {}}, p]}}`, which never moves -- behind a `$set`, a `$sort` or a
`$project` -- while the rest of its `$match` does. Compared are the parsed
query, the stages listed after the read, what each returned at
`executionStats`, and the documents. Measured on 8.2.11, 2026-11-14.
"""

from __future__ import annotations

import uuid
from typing import Any

from tests.mongod_live.conftest import same

GROUP = {"$group": {"_id": "$b"}}

PIPELINES: dict[str, list[dict]] = {
    "replace_with_root": [{"$replaceWith": "$$ROOT"}, {"$match": {"a": 1}}, {"$sort": {"b": 1}}],
    "replace_root_current": [
        GROUP,
        {"$replaceRoot": {"newRoot": "$$CURRENT"}},
        {"$sort": {"_id": 1}},
    ],
    "sort_limit": [GROUP, {"$sort": {"_id": 1}}, {"$limit": 2}],
    "sort_project_limit": [
        GROUP,
        {"$sort": {"_id": 1}},
        {"$project": {"z": "$_id"}},
        {"$limit": 2},
    ],
    "sort_skip_limit": [GROUP, {"$sort": {"_id": -1}}, {"$skip": 1}, {"$limit": 2}],
    "sort_two_limits": [
        GROUP,
        {"$sort": {"_id": 1}},
        {"$limit": 3},
        {"$addFields": {"q": 1}},
        {"$limit": 2},
    ],
    "sort_lookup_limit": [
        GROUP,
        {"$sort": {"_id": 1}},
        {
            "$lookup": {
                "from": "other",
                "let": {"k": "$_id"},
                "pipeline": [{"$match": {"$expr": {"$eq": ["$b", "$$k"]}}}],
                "as": "j",
            }
        },
        {"$limit": 2},
    ],
    "sort_unwind_limit": [
        {"$group": {"_id": "$a", "t": {"$push": "$b"}}},
        {"$sort": {"_id": 1}},
        {"$unwind": "$t"},
        {"$limit": 2},
    ],
    "replace_root_sort_limit": [
        {"$replaceRoot": {"newRoot": {"q": "$b"}}},
        {"$sort": {"q": -1}},
        {"$limit": 2},
    ],
    "facet": [
        {
            "$facet": {
                "f": [
                    {"$replaceWith": "$$ROOT"},
                    {"$sort": {"b": -1}},
                    {"$project": {"b": 1}},
                    {"$limit": 2},
                ]
            }
        }
    ],
    "rename": [{"$set": {"q": "$b"}}, {"$match": {"q": {"$gte": 2}}}, {"$sort": {"b": 1}}],
    "rename_onto_itself": [{"$set": {"b": "$a"}}, {"$match": {"b": 1}}, {"$sort": {"_id": 1}}],
    "rename_root_variable": [{"$set": {"q": "$$ROOT.b"}}, {"$match": {"q": 3}}],
    "rename_and_computed": [{"$set": {"q": "$b", "w": 5}}, {"$match": {"q": 1, "w": 5}}],
    "rename_in_an_or": [{"$set": {"q": "$b"}}, {"$match": {"$or": [{"q": 1}, {"a": 2}]}}],
    "rename_in_an_expr": [{"$addFields": {"q": "$b"}}, {"$match": {"$expr": {"$gt": ["$q", 3]}}}],
    "no_rename_dotted": [{"$set": {"q": "$x.a"}}, {"$match": {"q": 1}}],
    "sample_rate_query": [{"$match": {"a": 1, "$sampleRate": 1.0}}],
    "sample_rate_behind_set": [{"$set": {"c": 1}}, {"$match": {"a": 1, "$sampleRate": 0.5}}],
    "sample_rate_behind_sort": [GROUP, {"$sort": {"_id": 1}}, {"$match": {"$sampleRate": 0.5}}],
    "rand_behind_set": [{"$set": {"c": 1}}, {"$match": {"$expr": {"$lt": [{"$rand": {}}, 0.5]}}}],
}

# The documents of a pipeline that draws random numbers differ run to run.
RANDOM = {"sample_rate_behind_set", "sample_rate_behind_sort", "rand_behind_set"}


def _listed(stage: dict, random: bool) -> dict:
    """A stage as listed, and what it returned unless it drew random
    numbers; its time and memory are the machine's (and a top-k sort's size
    is RM-685). A `$facet`'s stages are listed alike."""
    head = next(iter(stage))
    keep = (head,) if random else (head, "nReturned")
    out = {k: v for k, v in stage.items() if k in keep}
    if head == "$facet":
        out[head] = {
            field: [_listed(s, random) for s in stages] for field, stages in stage[head].items()
        }
    return out


def _explained(client: Any) -> dict:
    db = client["live_rewrites_" + uuid.uuid4().hex[:8]]
    db.c.insert_many([{"_id": i, "x": {"a": i % 3}, "a": i % 3, "b": i} for i in range(6)])
    db.other.insert_many([{"_id": i, "b": i % 4} for i in range(4)])
    out = {}
    for name, pipeline in PIPELINES.items():
        reply = db.command(
            "explain",
            {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            verbosity="executionStats",
        )
        if "stages" in reply:
            planner = reply["stages"][0]["$cursor"]["queryPlanner"]
            after = [_listed(s, name in RANDOM) for s in reply["stages"][1:]]
        else:
            planner, after = reply["queryPlanner"], []
        out[name] = {"parsedQuery": planner["parsedQuery"], "after": after}
        if name not in RANDOM:
            out[name]["docs"] = list(db.c.aggregate(pipeline))
    client.drop_database(db.name)
    return out


def test_explain_lists_the_rewrites_mongod_makes(rust_mongo: Any, mongod: Any) -> None:
    ours, theirs = _explained(rust_mongo), _explained(mongod)
    for name in PIPELINES:
        same(ours[name], theirs[name])
