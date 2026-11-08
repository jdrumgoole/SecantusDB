"""Gap tests for the Rust MongoDB server items found on 2026-11-08 while
working RM-330.

Each test asks mongod 8.2.11 live and asserts the Rust server gives the same
answer, reduced to a value or an error code.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _code(answer: tuple) -> tuple:
    return answer[:2]


@pytest.mark.backlog("RM-420")
def test_an_id_match_refuses_a_dollar_object_in_in(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses `{$match: {_id: {$in: [{$x: 1}]}}}` with 2
    "cannot nest $ under $in", as on any other path; the Rust server answers."""

    def run(client: Any) -> tuple:
        coll = client.rm330f.c
        coll.drop()
        coll.insert_one({"_id": 1})
        pipeline = [{"$match": {"_id": {"$in": [{"$x": 1}]}}}]
        return _code(mongo_answer(lambda: list(coll.aggregate(pipeline))))

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog("RM-421")
def test_all_takes_a_dollar_object_as_a_value(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 takes `{a: {$all: [{$x: 1}]}}` as an equality to that
    object (no documents match here); the Rust server refuses it with 2."""

    def run(client: Any) -> tuple:
        coll = client.rm330f.c
        coll.drop()
        coll.insert_one({"_id": 1, "a": [1]})
        return _code(mongo_answer(lambda: list(coll.find({"a": {"$all": [{"$x": 1}]}}))))

    assert run(rust_mongo) == run(mongod)
