from __future__ import annotations

import pytest

from fongodb.query import QueryError, matches


def test_empty_query_matches_anything() -> None:
    assert matches({"a": 1}, {})


def test_equality_top_level() -> None:
    assert matches({"a": 1}, {"a": 1})
    assert not matches({"a": 1}, {"a": 2})


def test_dotted_path() -> None:
    doc = {"a": {"b": {"c": 5}}}
    assert matches(doc, {"a.b.c": 5})
    assert not matches(doc, {"a.b.c": 6})


def test_array_element_equality() -> None:
    doc = {"tags": ["red", "blue", "green"]}
    assert matches(doc, {"tags": "red"})
    assert not matches(doc, {"tags": "yellow"})


def test_dotted_path_into_array_of_subdocs() -> None:
    doc = {"items": [{"sku": "a"}, {"sku": "b"}]}
    assert matches(doc, {"items.sku": "b"})
    assert not matches(doc, {"items.sku": "c"})


def test_array_index_path() -> None:
    doc = {"vals": [10, 20, 30]}
    assert matches(doc, {"vals.1": 20})
    assert not matches(doc, {"vals.1": 10})


def test_comparison_operators() -> None:
    doc = {"age": 30}
    assert matches(doc, {"age": {"$gt": 20}})
    assert matches(doc, {"age": {"$gte": 30}})
    assert matches(doc, {"age": {"$lt": 31}})
    assert not matches(doc, {"age": {"$lt": 30}})


def test_in_and_nin() -> None:
    assert matches({"a": 2}, {"a": {"$in": [1, 2, 3]}})
    assert not matches({"a": 4}, {"a": {"$in": [1, 2, 3]}})
    assert matches({"a": 4}, {"a": {"$nin": [1, 2, 3]}})


def test_exists() -> None:
    assert matches({"a": None}, {"a": {"$exists": True}})
    assert matches({}, {"a": {"$exists": False}})
    assert not matches({}, {"a": {"$exists": True}})


def test_null_matches_missing() -> None:
    assert matches({}, {"a": None})
    assert matches({"a": None}, {"a": None})


def test_and_or_nor() -> None:
    doc = {"a": 1, "b": 2}
    assert matches(doc, {"$and": [{"a": 1}, {"b": 2}]})
    assert not matches(doc, {"$and": [{"a": 1}, {"b": 3}]})
    assert matches(doc, {"$or": [{"a": 99}, {"b": 2}]})
    assert matches(doc, {"$nor": [{"a": 99}, {"b": 99}]})


def test_not_at_field_level() -> None:
    assert matches({"a": 5}, {"a": {"$not": {"$gt": 10}}})
    assert not matches({"a": 50}, {"a": {"$not": {"$gt": 10}}})


def test_unknown_operator_raises() -> None:
    with pytest.raises(QueryError):
        matches({"a": 1}, {"a": {"$weirdo": 1}})
