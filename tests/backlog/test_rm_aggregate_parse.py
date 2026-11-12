"""`aggregate` fields mongod's parser checks that this server does not, or
reports otherwise (measured 8.2.11, 2026-11-09, found working RM-431)."""

from __future__ import annotations

import datetime
from typing import Any

import pytest
from bson import SON, Binary, Code, Decimal128, Int64, MinKey, ObjectId, Regex, Timestamp

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


@pytest.mark.backlog_fixed("RM-463")
@pytest.mark.parametrize("command", _MISTYPED, ids=[repr(c) for c in _MISTYPED])
def test_aggregate_type_checks_write_concern_and_bypass(
    rust_mongo: Any, mongod: Any, command: list
) -> None:
    """mongod refuses a non-object `writeConcern` and a non-bool-or-number
    `bypassDocumentValidation` on `aggregate` (14); this server ignored both
    and answered ok, until RM-463 parsed them in the field pass."""
    assert _answer(rust_mongo, command) == _answer(mongod, command)


_BAD_NAMESPACE = [
    [("aggregate", True), *_VALID],
    [("aggregate", None), *_VALID],
    [("aggregate", SON()), *_VALID],
    [("aggregate", []), *_VALID],
    [("aggregate", ObjectId("000000000000000000000000")), *_VALID],
    [("aggregate", Binary(b"x")), *_VALID],
    [("aggregate", Timestamp(1, 1)), *_VALID],
    [("aggregate", MinKey()), *_VALID],
    [("aggregate", Code("x")), *_VALID],
    [("aggregate", Regex("x")), *_VALID],
    [("aggregate", datetime.datetime(2020, 1, 1)), *_VALID],
    # A number is the collectionless form only when it equals 1, compared
    # by value: `1.5` (and Decimal128 `1.5`) used to be truncated to 1.
    [("aggregate", 1.5), *_VALID],
    [("aggregate", Decimal128("1.5")), *_VALID],
    [("aggregate", float("nan")), *_VALID],
    [("aggregate", 2), *_VALID],
    [("aggregate", Int64(1)), *_VALID],
    [("aggregate", 1.0), *_VALID],
    [("aggregate", Decimal128("1.0")), *_VALID],
    # The namespace is resolved after the field pass, before `cursor` is
    # asked for.
    [("aggregate", True), ("pipeline", [])],
    [("aggregate", True), ("pipeline", 5), ("cursor", {})],
]


@pytest.mark.backlog_fixed("RM-464")
@pytest.mark.parametrize("command", _BAD_NAMESPACE, ids=[repr(c) for c in _BAD_NAMESPACE])
def test_aggregate_names_a_mistyped_namespace(rust_mongo: Any, mongod: Any, command: list) -> None:
    """An `aggregate` value that is neither a string nor a number is
    InvalidNamespace (73) "Collection name has invalid type <t>" on mongod;
    this server answered BadValue (2), mongod's error for a number other
    than 1, and read a fractional number such as `1.5` as the
    collectionless 1."""
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
