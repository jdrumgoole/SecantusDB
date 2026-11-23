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


@pytest.mark.backlog_fixed("RM-620")
def test_root_does_not_show_the_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps the sort key as metadata, which `$$ROOT` does not
    show; this server's hidden field showed in an `$addFields` and a
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


@pytest.mark.backlog_fixed("RM-621")
def test_a_windows_output_reads_the_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """Behind a `$sort`, mongod 8.2.11 pushes each document's sort key in a
    window's output; this server refused it in `$setWindowFields` and read
    nothing in `$_internalSetWindowFields`. A sorting `$setWindowFields`
    reads the key of the `$sort` it runs behind."""
    push = {"x": {"$push": {"$meta": "sortKey"}}}
    pipelines = {
        "$setWindowFields": [
            SORT,
            {"$setWindowFields": {"output": push}},
            {"$project": {"x": 1}},
        ],
        "$setWindowFields sortBy": [
            SORT,
            {"$setWindowFields": {"partitionBy": "$k", "sortBy": {"n": -1}, "output": push}},
            {"$project": {"x": 1}},
        ],
        "$_internalSetWindowFields": [
            SORT,
            {"$_internalSetWindowFields": {"sortBy": {"n": 1}, "output": push}},
            {"$project": {"x": 1}},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog_fixed("RM-622")
def test_a_missing_new_root_is_missing(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 names a `newRoot` that resolves to nothing MISSING in
    its 40228; this server named it null."""
    pipelines = {
        "$replaceRoot": [{"$replaceRoot": {"newRoot": "$nope"}}],
        "$replaceWith": [{"$replaceWith": "$nope"}],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog_fixed("RM-622")
def test_a_stored_undefined_new_root_is_undefined(rust_mongo: Any, mongod: Any) -> None:
    """A `newRoot` that is a stored `undefined` is a value, not MISSING, in
    mongod 8.2.11's 40228."""
    from bson.raw_bson import RawBSONDocument

    def answer(client: Any) -> Any:
        client.drop_database("rmfound1113rm622")
        db = client.rmfound1113rm622
        # _id 1, u: undefined (type 0x06) -- no Python value encodes it.
        db.c.insert_one(
            RawBSONDocument(b"\x11\x00\x00\x00\x10_id\x00\x01\x00\x00\x00\x06u\x00\x00")
        )
        return mongo_answer(lambda: list(db.c.aggregate([{"$replaceRoot": {"newRoot": "$u"}}])))

    assert answer(rust_mongo) == answer(mongod)


@pytest.mark.backlog_fixed("RM-623")
def test_a_new_root_error_behind_a_sort_names_the_document(rust_mongo: Any, mongod: Any) -> None:
    """When a later stage reads the sort key, mongod 8.2.11's 40228 names
    the whole input document; this server named only the fields the
    `newRoot` reads, as both do with no reader of the key after."""
    pipelines = {
        "$sort, $replaceRoot, $meta": [
            SORT,
            {"$replaceRoot": {"newRoot": "$n"}},
            {"$addFields": {"m": {"$meta": "sortKey"}}},
        ]
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)
