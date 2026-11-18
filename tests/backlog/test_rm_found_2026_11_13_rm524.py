"""Gap tests found working RM-524 (2026-11-13): how a stage reads
`{$meta: "sortKey"}` where no `$sort` set one."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _answer(client: Any, pipeline: list) -> tuple:
    client.drop_database("rmfound1113rm524")
    db = client.rmfound1113rm524
    db.c.insert_many([{"_id": 1}, {"_id": 2}])
    return mongo_answer(lambda: list(db.c.aggregate(pipeline)))


@pytest.mark.backlog_fixed("RM-633")
def test_a_match_reads_a_missing_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """With no `$sort`, a `$match` reads the sort key as missing: mongod
    8.2.11's 17124 says "of type: missing" in its executor-error envelope,
    as a `$match` reading a missing field does. This server said
    `undefined`, with no envelope: it folded `$meta` as a constant."""
    pipeline = [{"$match": {"$expr": {"$gt": [{"$size": {"$meta": "sortKey"}}, 0]}}}]
    assert _answer(rust_mongo, pipeline) == _answer(mongod, pipeline)


@pytest.mark.backlog("RM-634")
def test_a_sort_key_projection_without_a_sort(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses a `$project` of `{$meta: "sortKey"}` with no
    `$sort` ahead of it, code 2 "... cannot use sortKey $meta projection
    without a sort"; this server's code 2 calls the pipeline unsupported."""
    pipeline = [{"$project": {"t": {"$type": {"$meta": "sortKey"}}}}]
    assert _answer(rust_mongo, pipeline) == _answer(mongod, pipeline)


@pytest.mark.backlog("RM-722")
def test_a_match_on_a_constant_remove_fails_to_optimize(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 folds `{$size: "$$REMOVE"}` in a leading `$match`'s
    `$expr` and fails under "Failed to optimize pipeline"; this server
    reports it as an executor error."""
    pipeline = [{"$match": {"$expr": {"$gt": [{"$size": "$$REMOVE"}, 0]}}}]
    assert _answer(rust_mongo, pipeline) == _answer(mongod, pipeline)
