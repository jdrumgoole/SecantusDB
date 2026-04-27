from __future__ import annotations

import pytest

from fongodb.expressions import ExpressionError, evaluate


def test_literal_passthrough() -> None:
    assert evaluate(5, {}) == 5
    assert evaluate("hello", {}) == "hello"
    assert evaluate(None, {}) is None
    assert evaluate(True, {}) is True


def test_field_path() -> None:
    doc = {"a": 1, "nested": {"b": 2}}
    assert evaluate("$a", doc) == 1
    assert evaluate("$nested.b", doc) == 2
    assert evaluate("$missing", doc) is None


def test_literal_op_blocks_recursion() -> None:
    assert evaluate({"$literal": "$a"}, {"a": 1}) == "$a"


def test_arithmetic() -> None:
    doc = {"x": 3, "y": 4}
    assert evaluate({"$add": ["$x", "$y", 10]}, doc) == 17
    assert evaluate({"$subtract": ["$y", "$x"]}, doc) == 1
    assert evaluate({"$multiply": ["$x", "$y"]}, doc) == 12
    assert evaluate({"$divide": ["$y", "$x"]}, doc) == pytest.approx(4 / 3)
    assert evaluate({"$mod": [10, 3]}, doc) == 1


def test_arithmetic_with_null_returns_null() -> None:
    assert evaluate({"$add": ["$missing", 5]}, {}) is None
    assert evaluate({"$divide": [5, 0]}, {}) is None


def test_concat() -> None:
    assert evaluate({"$concat": ["hello", " ", "world"]}, {}) == "hello world"
    assert evaluate({"$concat": ["x=", "$x"]}, {"x": 5}) == "x=5"


def test_comparisons() -> None:
    doc = {"a": 5}
    assert evaluate({"$eq": ["$a", 5]}, doc) is True
    assert evaluate({"$ne": ["$a", 5]}, doc) is False
    assert evaluate({"$gt": ["$a", 4]}, doc) is True
    assert evaluate({"$lt": ["$a", 4]}, doc) is False


def test_logical() -> None:
    assert evaluate({"$and": [True, True]}, {}) is True
    assert evaluate({"$and": [True, False]}, {}) is False
    assert evaluate({"$or": [False, True]}, {}) is True
    assert evaluate({"$not": [False]}, {}) is True


def test_cond_dict_form() -> None:
    doc = {"x": 5}
    expr = {"$cond": {"if": {"$gt": ["$x", 3]}, "then": "big", "else": "small"}}
    assert evaluate(expr, doc) == "big"


def test_cond_array_form() -> None:
    doc = {"x": 1}
    expr = {"$cond": [{"$gt": ["$x", 3]}, "big", "small"]}
    assert evaluate(expr, doc) == "small"


def test_if_null() -> None:
    assert evaluate({"$ifNull": ["$missing", "default"]}, {}) == "default"
    assert evaluate({"$ifNull": ["$present", "default"]}, {"present": 1}) == 1


def test_size() -> None:
    assert evaluate({"$size": "$tags"}, {"tags": [1, 2, 3]}) == 3


def test_string_case() -> None:
    assert evaluate({"$toUpper": "$s"}, {"s": "hello"}) == "HELLO"
    assert evaluate({"$toLower": "$s"}, {"s": "HELLO"}) == "hello"


def test_unsupported_operator_raises() -> None:
    with pytest.raises(ExpressionError):
        evaluate({"$bogus": 1}, {})


def test_dict_with_no_dollar_key_evaluates_each_field() -> None:
    out = evaluate({"a": "$x", "b": {"$add": ["$x", 1]}}, {"x": 5})
    assert out == {"a": 5, "b": 6}
