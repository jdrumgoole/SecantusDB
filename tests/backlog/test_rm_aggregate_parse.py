"""`aggregate` fields mongod's parser checks that this server does not, or
reports otherwise (measured 8.2.11, 2026-11-09, found working RM-431)."""

from __future__ import annotations

from typing import Any

import pytest
from bson import SON

from tests.backlog.conftest import mongo_answer


def _answer(client: Any, command: list) -> tuple:
    db = client["rmaggparse"]
    db.x.replace_one({"_id": 1}, {"_id": 1}, upsert=True)
    answer = mongo_answer(lambda: db.command(SON(command)))
    return answer if answer[0] == "err" else ("ok",)


_VALID = [("pipeline", []), ("cursor", {})]

_MISTYPED = [
    [("aggregate", "x"), *_VALID, ("writeConcern", 5)],
    [("aggregate", "x"), *_VALID, ("bypassDocumentValidation", "a")],
]


@pytest.mark.backlog("RM-463")
@pytest.mark.parametrize("command", _MISTYPED, ids=[repr(c) for c in _MISTYPED])
def test_aggregate_type_checks_write_concern_and_bypass(
    rust_mongo: Any, mongod: Any, command: list
) -> None:
    """mongod refuses a non-object `writeConcern` and a non-bool-or-number
    `bypassDocumentValidation` on `aggregate` (14); this server ignores both
    and answers ok."""
    assert _answer(rust_mongo, command) == _answer(mongod, command)


_BAD_NAMESPACE = [
    [("aggregate", True), *_VALID],
    [("aggregate", None), *_VALID],
    [("aggregate", SON()), *_VALID],
]


@pytest.mark.backlog("RM-464")
@pytest.mark.parametrize("command", _BAD_NAMESPACE, ids=[repr(c) for c in _BAD_NAMESPACE])
def test_aggregate_names_a_mistyped_namespace(rust_mongo: Any, mongod: Any, command: list) -> None:
    """An `aggregate` value that is neither a string nor a number is
    InvalidNamespace (73) "Collection name has invalid type <t>" on mongod;
    this server answers BadValue (2), mongod's error for a number other
    than 1."""
    assert _answer(rust_mongo, command) == _answer(mongod, command)


_TYPE_LISTS = [
    [("aggregate", "x"), ("pipeline", []), ("cursor", {"batchSize": "a"})],
    [("aggregate", "x"), *_VALID, ("maxTimeMS", "a")],
    [("find", "x"), ("batchSize", "a")],
]


@pytest.mark.backlog("RM-465")
@pytest.mark.parametrize("command", _TYPE_LISTS, ids=[repr(c) for c in _TYPE_LISTS])
def test_wrong_type_lists_the_types_in_mongods_order(
    rust_mongo: Any, mongod: Any, command: list
) -> None:
    """mongod's "is the wrong type ... expected types '[...]'" lists the
    types in an order of its own per field (`cursor.batchSize` "[double,
    decimal, long, int]", `aggregate.maxTimeMS` "[double, int, long,
    decimal]", `FindCommandRequest.batchSize` "[decimal, double, long,
    int]"); this server lists all three "[decimal, int, double, long]"."""
    assert _answer(rust_mongo, command) == _answer(mongod, command)
