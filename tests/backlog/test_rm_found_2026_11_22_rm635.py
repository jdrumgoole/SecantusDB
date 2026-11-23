"""Gap tests found working RM-635 (2026-11-22): what a stored BSON
`undefined` still does that mongod 8.2.11's does not.

pymongo cannot encode `undefined`, so the documents are raw BSON.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_found_2026_11_13_rm525 import DOCS, SORT


def _answers(client: Any, pipelines: dict[str, list]) -> dict:
    client.drop_database("rmfound1122rm635")
    db = client.rmfound1122rm635
    db.c.insert_many(DOCS)
    return {
        name: mongo_answer(lambda p=p: list(db.c.aggregate(p))) for name, p in pipelines.items()
    }


@pytest.mark.backlog_fixed("RM-761")
def test_arithmetic_over_a_stored_undefined_is_null(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 treats a stored `undefined` as null in `$add`: the sum
    is null. This server refused it with code 14 ("$add only supports
    numeric or date types"; RM-761, closed by RM-763's MISSING case)."""
    pipelines = {
        "$add": [SORT, {"$project": {"t": {"$add": ["$a", 1]}}}],
        "$multiply": [SORT, {"$project": {"t": {"$multiply": ["$a", 2]}}}],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


def _let_pipeline(ids: list[int]) -> list:
    return [
        {"$match": {"_id": {"$in": ids}}},
        SORT,
        {
            "$lookup": {
                "from": "c",
                "let": {"v": "$a"},
                "pipeline": [{"$limit": 1}, {"$project": {"t": {"$type": "$$v"}}}],
                "as": "l",
            }
        },
        {"$project": {"l": 1}},
    ]


@pytest.mark.backlog_fixed("RM-762")
def test_a_lookup_let_variable_keeps_a_stored_undefined(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$lookup` `let` variable bound from a stored
    `undefined` reads "undefined" in `$type` (and one bound from null reads
    "null"). This server read "missing" (RM-762, closed by RM-763)."""
    pipelines = {"$lookup let": _let_pipeline([1, 2])}
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-763")
def test_a_lookup_let_variable_bound_from_an_absent_field_is_missing(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's `$lookup` `let` variable bound from an absent field
    reads "missing" in `$type`. This server binds null: the variables of a
    sub-pipeline are a document of values, which has no MISSING."""
    pipelines = {"$lookup let": _let_pipeline([3])}
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)
