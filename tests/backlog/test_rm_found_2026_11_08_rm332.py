"""Gap tests for what was found while working RM-332 (the sort refusals),
each asserting mongod's answer."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _meta_sort(client: Any) -> tuple:
    coll = client.rm332found.c
    coll.drop()
    coll.insert_many([{"_id": i} for i in range(5)])
    return mongo_answer(
        lambda: list(coll.aggregate([{"$sort": {"s": {"$meta": "randVal"}}}, {"$count": "n"}]))
    )


@pytest.mark.backlog_fixed("RM-423")
def test_a_sort_on_a_meta_key_runs(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.1 sorts by `{$meta: "randVal"}` in a `$sort` stage; the
    Rust server refused the stage as unsupported."""
    assert _meta_sort(rust_mongo) == _meta_sort(mongod)


def _search_score_sort(client: Any) -> tuple:
    coll = client.rm332found.c
    coll.drop()
    coll.insert_many([{"_id": i, "a": i % 2} for i in range(6)])
    return mongo_answer(
        lambda: [
            d["_id"] for d in coll.aggregate([{"$sort": {"a": -1, "s": {"$meta": "searchScore"}}}])
        ]
    )


@pytest.mark.backlog_fixed("RM-423")
def test_a_search_score_sort_key_ties(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sorts `{a: -1, s: {$meta: "searchScore"}}` by `a` alone,
    the documents tying on the score they have none of; the Rust server
    refused the stage."""
    assert _search_score_sort(rust_mongo) == _search_score_sort(mongod)


def _rand_sort_then_project(client: Any) -> tuple:
    coll = client.rm332found.c
    coll.drop()
    coll.insert_many([{"_id": i, "a": i % 2} for i in range(6)])
    return mongo_answer(
        lambda: [
            d["a"]
            for d in coll.aggregate(
                [
                    {"$sort": {"a": 1, "s": {"$meta": "randVal"}}},
                    {"$project": {"a": 1, "_id": 0}},
                ]
            )
        ]
    )


@pytest.mark.backlog("RM-691")
def test_a_rand_sort_beside_a_field_under_a_project_is_refused(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 refuses `{$sort: {a: 1, s: {$meta: "randVal"}}}`
    followed by a `$project` with 16410 (a `$computed1` field path its
    pipeline rewrite makes); the Rust server sorts it."""
    assert _rand_sort_then_project(rust_mongo) == _rand_sort_then_project(mongod)
