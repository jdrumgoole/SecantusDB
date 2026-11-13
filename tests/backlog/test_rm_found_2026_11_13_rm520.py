"""Gap tests found working RM-520 (2026-11-13): where the sort key this
server keeps as a hidden field shows, or is not there to read, and two
`$replaceRoot` error messages probed beside it."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

DOCS = [{"_id": i, "n": n, "k": k} for i, (n, k) in enumerate([(3, 1), (1, 0), (2, 1), (2, 0)])]

SORT = {"$sort": {"k": 1}}


def _answers(client: Any, pipelines: dict[str, list]) -> dict:
    client.drop_database("rmfound1113rm520")
    db = client.rmfound1113rm520
    db.c.insert_many(DOCS)
    return {
        name: mongo_answer(lambda p=p: list(db.c.aggregate(p))) for name, p in pipelines.items()
    }


@pytest.mark.backlog("RM-620")
def test_root_does_not_show_the_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps the sort key as metadata, which `$$ROOT` does not
    show; this server's hidden field shows in an `$addFields` and a
    `$lookup`'s `let`."""
    meta = {"$meta": "sortKey"}
    pipelines = {
        "$addFields": [
            SORT,
            {"$addFields": {"r": "$$ROOT"}},
            {"$project": {"r": 1, "m": meta}},
        ],
        "$lookup let": [
            SORT,
            {
                "$lookup": {
                    "from": "c",
                    "let": {"r": "$$ROOT"},
                    "pipeline": [{"$limit": 1}, {"$replaceWith": "$$r"}],
                    "as": "j",
                }
            },
            {"$addFields": {"m": meta}},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-621")
def test_a_windows_output_reads_the_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """Behind a `$sort`, mongod 8.2.11 pushes each document's sort key in a
    window's output; this server refuses it in `$setWindowFields` and reads
    null in `$_internalSetWindowFields`."""
    push = {"x": {"$push": {"$meta": "sortKey"}}}
    pipelines = {
        "$setWindowFields": [
            SORT,
            {"$setWindowFields": {"output": push}},
            {"$project": {"x": 1}},
        ],
        "$_internalSetWindowFields": [
            SORT,
            {"$_internalSetWindowFields": {"sortBy": {"n": 1}, "output": push}},
            {"$project": {"x": 1}},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-622")
def test_a_missing_new_root_is_missing(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 names a `newRoot` that resolves to nothing MISSING in
    its 40228; this server names it null."""
    pipelines = {
        "$replaceRoot": [{"$replaceRoot": {"newRoot": "$nope"}}],
        "$replaceWith": [{"$replaceWith": "$nope"}],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-623")
def test_a_new_root_error_behind_a_sort_names_the_document(rust_mongo: Any, mongod: Any) -> None:
    """When a later stage reads the sort key, mongod 8.2.11's 40228 names
    the whole input document; this server names the fields the `newRoot`
    reads, as both do with no reader of the key after."""
    pipelines = {
        "$sort, $replaceRoot, $meta": [
            SORT,
            {"$replaceRoot": {"newRoot": "$n"}},
            {"$addFields": {"m": {"$meta": "sortKey"}}},
        ]
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)
