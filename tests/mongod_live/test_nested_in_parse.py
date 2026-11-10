"""A `$in` / `$nin` element that is a `$`-prefixed document is a parse error
on mongod 8.2 ("cannot nest $ under $in", 2), on every command that takes a
filter and wherever the filter holds it. The Rust server refused it only
from a clause its matcher evaluated, so an `_id` read, an `$or` branch that
was never reached, an `$elemMatch` over a missing field or a write that
matched nothing answered (RM-420, measured 8.2.11 on 2026-11-10)."""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest

from tests.mongod_live.conftest import mongo_answer, same

BAD = {"$in": [{"$x": 1}]}

#: (name, call) -- each run over a collection holding `{_id: 1}`.
CALLS: list[tuple[str, Callable[[Any], Any]]] = [
    ("aggregate on _id", lambda c: list(c.aggregate([{"$match": {"_id": BAD}}]))),
    (
        "aggregate $or past a matching branch",
        lambda c: list(c.aggregate([{"$match": {"$or": [{"_id": 1}, {"b": BAD}]}}])),
    ),
    (
        "aggregate $match after $project",
        lambda c: list(c.aggregate([{"$project": {"_id": 1}}, {"$match": {"_id": BAD}}])),
    ),
    ("find on _id", lambda c: list(c.find({"_id": BAD}))),
    ("find $or past a matching branch", lambda c: list(c.find({"$or": [{"_id": 1}, {"b": BAD}]}))),
    ("find under $elemMatch", lambda c: list(c.find({"b": {"$elemMatch": {"c": BAD}}}))),
    ("find under a false sibling", lambda c: list(c.find({"_id": {"$gt": 5, **BAD}}))),
    ("find $nin", lambda c: list(c.find({"b": {"$nin": [{"$x": 1}]}}))),
    (
        "update matching nothing",
        lambda c: c.update_one({"_id": 9, "b": BAD}, {"$set": {"z": 1}}).raw_result["n"],
    ),
    ("delete on _id", lambda c: c.delete_one({"_id": BAD}).deleted_count),
    ("count", lambda c: c.count_documents({"_id": BAD})),
    ("distinct", lambda c: c.distinct("_id", {"_id": BAD})),
    ("findAndModify", lambda c: c.find_one_and_update({"_id": BAD}, {"$set": {"z": 1}})),
    ("a DBRef is a value", lambda c: list(c.find({"_id": {"$in": [{"$ref": "c", "$id": 1}]}}))),
]


@pytest.mark.parametrize("name, call", CALLS, ids=[n for n, _ in CALLS])
def test_a_dollar_object_in_in_is_refused_as_mongod_refuses_it(
    rust_mongo: Any, mongod: Any, name: str, call: Callable[[Any], Any]
) -> None:
    def run(client: Any) -> tuple:
        coll = client.nested_in_parse.c
        coll.drop()
        coll.insert_one({"_id": 1})
        return mongo_answer(lambda: call(coll))

    same(run(rust_mongo), run(mongod))
