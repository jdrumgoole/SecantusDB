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


def test_date_extractors() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 27, 13, 14, 15)
    doc = {"ts": when}
    assert evaluate({"$year": "$ts"}, doc) == 2026
    assert evaluate({"$month": "$ts"}, doc) == 4
    assert evaluate({"$dayOfMonth": "$ts"}, doc) == 27
    assert evaluate({"$hour": "$ts"}, doc) == 13
    assert evaluate({"$minute": "$ts"}, doc) == 14
    assert evaluate({"$second": "$ts"}, doc) == 15


def test_date_to_string_default() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 27, 13, 14, 15, 678000)
    out = evaluate({"$dateToString": {"date": "$ts"}}, {"ts": when})
    assert out == "2026-04-27T13:14:15.678Z"


def test_date_to_string_custom_format() -> None:
    import datetime as dt

    when = dt.datetime(2026, 4, 27)
    out = evaluate({"$dateToString": {"date": "$ts", "format": "%Y/%m/%d"}}, {"ts": when})
    assert out == "2026/04/27"


def test_array_elem_at() -> None:
    doc = {"a": [10, 20, 30]}
    assert evaluate({"$arrayElemAt": ["$a", 0]}, doc) == 10
    assert evaluate({"$arrayElemAt": ["$a", -1]}, doc) == 30
    assert evaluate({"$arrayElemAt": ["$a", 99]}, doc) is None


def test_first_last_slice_reverse() -> None:
    doc = {"a": [1, 2, 3, 4]}
    assert evaluate({"$first": "$a"}, doc) == 1
    assert evaluate({"$last": "$a"}, doc) == 4
    assert evaluate({"$slice": ["$a", 2]}, doc) == [1, 2]
    assert evaluate({"$slice": ["$a", -2]}, doc) == [3, 4]
    assert evaluate({"$slice": ["$a", 1, 2]}, doc) == [2, 3]
    assert evaluate({"$reverseArray": "$a"}, doc) == [4, 3, 2, 1]


def test_concat_arrays() -> None:
    out = evaluate({"$concatArrays": [[1, 2], [3], [4, 5]]}, {})
    assert out == [1, 2, 3, 4, 5]


def test_in_operator_in_expressions() -> None:
    assert evaluate({"$in": ["b", ["a", "b", "c"]]}, {}) is True
    assert evaluate({"$in": ["x", ["a", "b", "c"]]}, {}) is False


def test_to_int_conversions() -> None:
    assert evaluate({"$toInt": 3.7}, {}) == 3
    assert evaluate({"$toInt": "42"}, {}) == 42
    assert evaluate({"$toInt": True}, {}) == 1


def test_to_double_conversions() -> None:
    assert evaluate({"$toDouble": 3}, {}) == 3.0
    assert evaluate({"$toDouble": "3.14"}, {}) == 3.14


def test_to_bool_conversions() -> None:
    assert evaluate({"$toBool": 0}, {}) is False
    assert evaluate({"$toBool": "x"}, {}) is True
    assert evaluate({"$toBool": ""}, {}) is False


def test_filter_basic() -> None:
    expr = {"$filter": {"input": "$nums", "as": "n", "cond": {"$gt": ["$$n", 2]}}}
    assert evaluate(expr, {"nums": [1, 2, 3, 4]}) == [3, 4]


def test_filter_with_default_var_name() -> None:
    expr = {"$filter": {"input": "$nums", "cond": {"$gte": ["$$this", 2]}}}
    assert evaluate(expr, {"nums": [1, 2, 3]}) == [2, 3]


def test_filter_with_limit() -> None:
    expr = {
        "$filter": {
            "input": "$nums",
            "as": "n",
            "cond": {"$gt": ["$$n", 0]},
            "limit": 2,
        }
    }
    assert evaluate(expr, {"nums": [1, 2, 3, 4]}) == [1, 2]


def test_map_transforms_each_element() -> None:
    expr = {"$map": {"input": "$nums", "as": "n", "in": {"$multiply": ["$$n", 10]}}}
    assert evaluate(expr, {"nums": [1, 2, 3]}) == [10, 20, 30]


def test_reduce_sums() -> None:
    expr = {
        "$reduce": {
            "input": "$nums",
            "initialValue": 0,
            "in": {"$add": ["$$value", "$$this"]},
        }
    }
    assert evaluate(expr, {"nums": [1, 2, 3, 4]}) == 10


def test_reduce_concat_string() -> None:
    expr = {
        "$reduce": {
            "input": "$tags",
            "initialValue": "",
            "in": {"$concat": ["$$value", "$$this"]},
        }
    }
    assert evaluate(expr, {"tags": ["a", "b", "c"]}) == "abc"


def test_root_system_var_resolves_to_doc() -> None:
    out = evaluate("$$ROOT", {"x": 1})
    assert out == {"x": 1}


def test_unknown_system_var_raises() -> None:
    from fongodb.expressions import ExpressionError

    with pytest.raises(ExpressionError):
        evaluate("$$mystery", {})
