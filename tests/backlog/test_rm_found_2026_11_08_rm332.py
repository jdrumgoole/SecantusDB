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


@pytest.mark.backlog_fixed("RM-691")
def test_a_rand_sort_beside_a_field_under_a_project_is_refused(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 refuses `{$sort: {a: 1, s: {$meta: "randVal"}}}`
    followed by a `$project` with 16410 (a `$computed1` field path its
    pipeline rewrite makes); the Rust server sorted it."""
    assert _rand_sort_then_project(rust_mongo) == _rand_sort_then_project(mongod)


_R = {"$meta": "randVal"}
_SHAPES = {
    "exclusion": [{"$sort": {"a": 1, "s": _R}}, {"$project": {"b": 0}}],
    "rand_only": [{"$sort": {"s": _R}}, {"$project": {"a": 1}}],
    "no_project": [{"$sort": {"a": 1, "s": _R}}],
    "add_fields": [{"$sort": {"a": 1, "s": _R}}, {"$addFields": {"x": 1}}],
    "group_field": [{"$sort": {"a": 1, "s": _R}}, {"$group": {"_id": "$a"}}],
    "group_const": [{"$sort": {"a": 1, "s": _R}}, {"$group": {"_id": None, "n": {"$sum": 1}}}],
    "limit": [{"$sort": {"a": 1, "s": _R}}, {"$limit": 3}, {"$project": {"a": 1}}],
    "skip": [{"$sort": {"a": 1, "s": _R}}, {"$skip": 0}, {"$project": {"a": 1}}],
    "match_after": [{"$sort": {"a": 1, "s": _R}}, {"$match": {"a": 1}}, {"$project": {"a": 1}}],
    "limit_before": [{"$limit": 5}, {"$sort": {"a": 1, "s": _R}}, {"$project": {"a": 1}}],
    "project_meta": [{"$sort": {"a": 1, "s": _R}}, {"$project": {"a": 1, "r": _R}}],
    "unset": [{"$sort": {"a": 1, "s": _R}}, {"$unset": "b"}],
    "unwind": [{"$sort": {"s": _R}}, {"$unwind": "$b"}, {"$project": {"a": 1}}],
    "replace_root": [{"$sort": {"s": _R}}, {"$replaceRoot": {"newRoot": {"x": "$a"}}}],
    "search_score": [{"$sort": {"a": 1, "s": {"$meta": "searchScore"}}}, {"$project": {"a": 1}}],
    "sort_sort": [{"$sort": {"s": _R}}, {"$sort": {"a": 1}}, {"$project": {"a": 1}}],
    "count": [{"$sort": {"a": 1, "s": _R}}, {"$count": "n"}],
}


def _meta_sort_shapes(client: Any) -> dict:
    db = client.rm332found
    db.c.drop()
    db.c.insert_many([{"_id": i, "a": i % 2, "b": i} for i in range(6)])
    db.nosuch.drop()
    out = {k: mongo_answer(lambda p=p: len(list(db.c.aggregate(p)))) for k, p in _SHAPES.items()}
    out["missing"] = mongo_answer(lambda: len(list(db.nosuch.aggregate(_SHAPES["rand_only"]))))
    return out


@pytest.mark.backlog_fixed("RM-691")
def test_meta_sort_pushdown_refusals_match_mongod(rust_mongo: Any, mongod: Any) -> None:
    """Which `$meta`-keyed `$sort` pipelines mongod 8.2.11 refuses with
    16410: those whose later stages read only some fields, with no `$limit`
    taken into the sort and no metadata read, on a collection that exists."""
    assert _meta_sort_shapes(rust_mongo) == _meta_sort_shapes(mongod)
