"""Gap tests for what working RM-215 (`$queryStats`) found on 2026-11-08,
comparing with mongod 8.2.11 (a one-member replica set, macOS arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

# ------------------------------------------ a collectionless $lookup --


def _collectionless_lookups(client: Any) -> dict:
    client.drop_database("rm215l")
    db = client.rm215l
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 2}])
    return {
        "aggregate_1": mongo_answer(
            lambda: list(
                client.admin.aggregate(
                    [
                        {"$documents": [{"x": 1}]},
                        {
                            "$lookup": {
                                "pipeline": [{"$documents": [{"y": 2}, {"y": 3}]}],
                                "as": "z",
                            }
                        },
                    ]
                )
            )
        ),
        "over_a_collection": mongo_answer(
            lambda: list(
                db.c.aggregate(
                    [
                        {
                            "$lookup": {
                                "pipeline": [{"$documents": [{"y": "$$a"}]}],
                                "let": {"a": "$a"},
                                "as": "z",
                            }
                        }
                    ]
                )
            )
        ),
        "without_documents": mongo_answer(
            lambda: list(db.c.aggregate([{"$lookup": {"pipeline": [{"$match": {}}], "as": "z"}}]))
        ),
    }


@pytest.mark.backlog_fixed("RM-413")
def test_a_lookup_without_from_runs_its_documents(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 runs a `$lookup` with no `from` whose pipeline starts
    with `$documents`, and refuses one whose pipeline does not (9, "$lookup
    stage without explicit collection must have a pipeline with $documents
    as first stage"). This server answered 2 "$lookup requires 'from'
    (string)" to both, until RM-413 ran the pipeline's own documents."""
    assert _collectionless_lookups(rust_mongo) == _collectionless_lookups(mongod_rs)


# ------------------------------- a collectionless aggregate's explain --


def _collectionless_explain(client: Any) -> Any:
    def stages() -> list:
        reply = client.admin.command(
            "explain",
            {
                "aggregate": 1,
                "pipeline": [{"$documents": [{"a": 1}, {"a": 2}]}, {"$match": {"a": 1}}],
                "cursor": {},
            },
            verbosity="executionStats",
        )
        # The time each stage took is the machine's, not the server's.
        return [
            {k: v for k, v in s.items() if k != "executionTimeMillisEstimate"}
            for s in reply["stages"]
        ]

    return mongo_answer(stages)


@pytest.mark.backlog_fixed("RM-414")
def test_a_collectionless_explain_counts_each_stage(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 explains `{aggregate: 1}` over `$documents` with each
    stage's `nReturned` (and `executionTimeMillisEstimate`), a `$queue`
    that shows no documents, and the `$match` moved before the
    `$replaceRoot` and rewritten over `_tempDocumentsField`. This server
    gave the stages without counts, `$queue: [{}]`, and the `$match` last
    as written, until RM-414 counted each stage and moved the `$match`."""
    assert _collectionless_explain(rust_mongo) == _collectionless_explain(mongod_rs)


_DOCS = {"$documents": [{"a": 1, "b": 3}, {"a": 2}]}
_THREE = {"$documents": [{"a": 1}, {"a": 2}, {"a": 3}, {"a": 4}]}

_COUNTED = {
    "an_and": [_DOCS, {"$match": {"a": 1, "b": {"$gt": 2}}}],
    "an_or": [_DOCS, {"$match": {"$or": [{"a": 1}, {"b": 3}]}}],
    "not_exists": [_DOCS, {"$match": {"a": {"$exists": False}}}],
    "a_nor": [_DOCS, {"$match": {"$nor": [{"a": 1}]}}],
    "an_in": [_DOCS, {"$match": {"a": {"$in": [1, 2]}}}],
    "a_regex": [_DOCS, {"$match": {"a": {"$regex": "x"}}}],
    "two_matches": [_DOCS, {"$match": {"a": 1}}, {"$match": {"b": 3}}],
    "past_add_fields": [_DOCS, {"$addFields": {"c": 1}}, {"$match": {"a": 1}}],
    "on_an_added_field": [_DOCS, {"$addFields": {"c": 1}}, {"$match": {"c": 1}}],
    "past_an_unwind": [_DOCS, {"$unwind": "$a"}, {"$match": {"b": 3}}],
    "an_elem_match": [_DOCS, {"$match": {"a": {"$elemMatch": {"$gt": 1}}}}],
    "always_false": [_DOCS, {"$match": {"$alwaysFalse": 1}}],
    "only_a_comment": [_DOCS, {"$match": {"$comment": "x"}}],
    "empty": [_DOCS, {"$match": {}}],
    "two_roots": [
        {"$documents": [{"x": {"y": {"a": 1}}}]},
        {"$replaceRoot": {"newRoot": "$x.y"}},
        {"$match": {"a": 1}},
    ],
    "a_skip": [_THREE, {"$project": {"a": 1}}, {"$skip": 1}],
    "skip_limit": [_THREE, {"$addFields": {"z": 1}}, {"$skip": 1}, {"$limit": 2}],
    "a_limit": [_THREE, {"$limit": 2}],
    "match_limit": [_THREE, {"$match": {"a": {"$gte": 1}}}, {"$limit": 1}],
    "match_skip_match": [_DOCS, {"$skip": 1}, {"$match": {"a": 2}}],
    "a_group": [_DOCS, {"$group": {"_id": "$a"}}, {"$match": {"_id": 1}}],
    "no_documents": [{"$documents": []}],
    "query_stats": [{"$queryStats": {}}],
    "sampled_queries": [{"$listSampledQueries": {}}],
}

_LISTED = {
    "local_sessions": [{"$listLocalSessions": {}}],
    "all_users": [{"$listLocalSessions": {"allUsers": True}}],
    "named_users": [{"$listLocalSessions": {"users": [{"user": "u", "db": "d"}]}}],
    "local_sessions_match": [
        {"$listLocalSessions": {}},
        {"$project": {"x": 1}},
        {"$match": {"x": 1}},
    ],
}


def _collectionless_stages(client: Any, pipelines: dict, verbosity: str) -> dict:
    def stages(pipeline: list) -> list:
        reply = client.admin.command(
            "explain",
            {"aggregate": 1, "pipeline": pipeline, "cursor": {}},
            verbosity=verbosity,
        )
        return [
            {k: v for k, v in s.items() if k != "executionTimeMillisEstimate"}
            for s in reply["stages"]
        ]

    return {name: mongo_answer(lambda p=p: stages(p)) for name, p in pipelines.items()}


@pytest.mark.backlog_fixed("RM-414")
def test_a_collectionless_explain_reorders_and_counts_as_mongod(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 moves a `$match` behind `$documents`' `$replaceRoot`
    ahead of it unless it holds an `$expr`, an `$elemMatch` or is only
    `$alwaysFalse` (an empty one moves as it is), moves a `$skip` ahead of
    the stages that transform one document into one, counts what each stage
    returns under a `$limit`'s pull (a `$skip` reads past what it skips),
    and lists `$listLocalSessions` with the users it lists. This server
    listed them as written and uncounted until RM-414."""
    assert _collectionless_stages(rust_mongo, _COUNTED, "executionStats") == (
        _collectionless_stages(mongod_rs, _COUNTED, "executionStats")
    )
    assert _collectionless_stages(rust_mongo, _LISTED, "queryPlanner") == (
        _collectionless_stages(mongod_rs, _LISTED, "queryPlanner")
    )
