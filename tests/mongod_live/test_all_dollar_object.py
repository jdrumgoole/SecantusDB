"""An `$all` element that is a `$`-prefixed document is refused by mongod 8.2
("no $ expressions in $all", 2) only when its first key is a path-accepting
keyword (`$eq`, `$gt`, `$exists`, the `$_internal*` ones, ...); any other
`$`-led document -- `{$x: 1}`, `{$not: 1}`, `{$geometry: 1}` -- is an equality
to that document. The Rust server refused every `$`-led element, and once it
took them as values its positional update and upsert seeding still read them
as operators (RM-421, measured 8.2.11 on 2026-11-10)."""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest

from tests.mongod_live.conftest import mongo_answer, same

X = {"$x": 1}

#: (name, call) -- each run over the three documents `run` inserts.
CALLS: list[tuple[str, Callable[[Any], Any]]] = [
    ("find", lambda c: sorted(d["_id"] for d in c.find({"a": {"$all": [X]}}))),
    ("find beside a scalar", lambda c: sorted(d["_id"] for d in c.find({"a": {"$all": [3, X]}}))),
    ("find $not is no keyword", lambda c: list(c.find({"a": {"$all": [{"$not": 1}]}}))),
    ("find $geometry is no keyword", lambda c: list(c.find({"a": {"$all": [{"$geometry": 1}]}}))),
    ("find $and is no keyword", lambda c: list(c.find({"a": {"$all": [{"$and": 1}]}}))),
    ("find $eq is refused", lambda c: list(c.find({"a": {"$all": [3, {"$eq": 1}]}}))),
    ("find $exists is refused", lambda c: list(c.find({"a": {"$all": [{"$exists": 1}]}}))),
    (
        "find $_internalSchemaEq is refused",
        lambda c: list(c.find({"a": {"$all": [{"$_internalSchemaEq": 1}]}})),
    ),
    (
        "aggregate",
        lambda c: sorted(d["_id"] for d in c.aggregate([{"$match": {"a": {"$all": [X]}}}])),
    ),
    ("count", lambda c: c.count_documents({"a": {"$all": [X]}})),
    (
        "positional update",
        lambda c: (
            c.update_one({"_id": 2, "a": {"$all": [X]}}, {"$set": {"a.$": 7}}).modified_count,
            c.find_one({"_id": 2}),
        ),
    ),
    (
        "upsert seeds the document",
        lambda c: c.find_one_and_update(
            {"_id": 9, "a": {"$all": [X]}},
            {"$set": {"b": 1}},
            upsert=True,
            return_document=True,
        ),
    ),
]


@pytest.mark.parametrize("name, call", CALLS, ids=[n for n, _ in CALLS])
def test_all_takes_a_dollar_object_as_mongod_does(
    rust_mongo: Any, mongod: Any, name: str, call: Callable[[Any], Any]
) -> None:
    def run(client: Any) -> tuple:
        coll = client.all_dollar_object.c
        coll.drop()
        coll.insert_many([{"_id": 1, "a": X}, {"_id": 2, "a": [X, 3]}, {"_id": 3, "a": 1}])
        return mongo_answer(lambda: call(coll))

    same(run(rust_mongo), run(mongod))
