"""Gap tests for the Rust MongoDB server items found on 2026-11-10 while
working RM-421.

Each test asks mongod 8.2.11 live and asserts the Rust server gives the same
answer, reduced to a value or an error code and message.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

#: A `$near` key where mongod reads a value, not an operator.
FILTERS: list[dict[str, Any]] = [
    {"a": {"$eq": {"$near": 1}}},
    {"a": {"$ne": {"$nearSphere": 1}}},
    {"a": {"x": {"$near": 1}}},
    {"a": {"$eq": {"x": {"$geoNear": 1}}}},
    {"a": {"$all": [{"x": {"$near": 1}}]}},
    {"a": {"$all": [{"$near": 1}]}},
]


@pytest.mark.backlog("RM-546")
def test_a_near_key_inside_a_value_is_an_equality(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers each filter as an equality (or, for `$all:
    [{$near: 1}]`, refuses it with `$all`'s own message); the Rust server
    refuses every one with 2 "geo $near must be top-level expr"."""

    def run(client: Any) -> list:
        coll = client.rm421f.c
        coll.drop()
        coll.insert_one({"_id": 1, "a": 1})
        return [mongo_answer(lambda f=f: [d["_id"] for d in coll.find(f)]) for f in FILTERS]

    assert run(rust_mongo) == run(mongod)
