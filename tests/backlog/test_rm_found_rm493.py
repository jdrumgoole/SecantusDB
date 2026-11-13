"""Found while working RM-493 (2026-11-13), against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest
from pymongo.errors import OperationFailure

from tests.backlog.opdebug import fresh

#: `"$"` in each position an expression parses an operand.
_SHAPES = [
    "$",
    {"$toUpper": "$"},
    {"$concat": ["$", "x"]},
    {"$_internalFindAllValuesAtPath": "$"},
]


def _answer(client: Any, expr: Any) -> Any:
    db = client[fresh("dollar")]
    db.c.insert_one({"_id": 1, "a": 1})
    try:
        return ("ok", list(db.c.aggregate([{"$project": {"_id": 0, "f": expr}}])))
    except OperationFailure as e:
        return ("error", e.code, (e.details or {}).get("errmsg"))


@pytest.mark.backlog("RM-594")
def test_a_bare_dollar_is_no_field_path(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 refuses `"$"` as an operand when it parses it (16872
    "'$' by itself is not a valid FieldPath"); this server reads it as a
    missing field: `{$toUpper: "$"}` is `""`, `{$concat: ["$", "x"]}` is
    `null`, and the bare `"$"` drops the field."""
    for expr in _SHAPES:
        theirs = _answer(mongod_rs, expr)
        assert theirs[:2] == ("error", 16872)
        assert _answer(rust_mongo, expr) == theirs, expr
