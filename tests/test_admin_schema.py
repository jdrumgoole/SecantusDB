"""Schema sampler unit tests."""

from __future__ import annotations

import datetime as dt

from bson import ObjectId

from secantus.admin.schema import summarize


def test_empty_input() -> None:
    out = summarize([])
    assert out["sample_size"] == 0
    assert out["fields"] == []


def test_top_level_fields_with_types() -> None:
    docs = [
        {"_id": ObjectId(), "name": "alice", "age": 30},
        {"_id": ObjectId(), "name": "bob", "age": 25},
        {"_id": ObjectId(), "name": None, "age": 40},
    ]
    out = summarize(docs)
    assert out["sample_size"] == 3
    by_path = {f["path"]: f for f in out["fields"]}

    assert by_path["_id"]["count"] == 3
    assert dict(by_path["_id"]["types"]).get("ObjectId") == 3

    assert by_path["age"]["count"] == 3
    assert dict(by_path["age"]["types"]).get("int") == 3
    # Top values include all three ints.
    assert {v for v, _ in by_path["age"]["top_values"]} == {30, 25, 40}

    assert by_path["name"]["count"] == 3
    assert by_path["name"]["null_count"] == 1


def test_nested_objects_use_dotted_paths() -> None:
    docs = [{"a": {"b": {"c": 1}}}, {"a": {"b": {"c": 2}}}]
    paths = {f["path"] for f in summarize(docs)["fields"]}
    assert "a" in paths
    assert "a.b" in paths
    assert "a.b.c" in paths


def test_arrays_descend_into_dicts() -> None:
    docs = [{"items": [{"x": 1}, {"x": 2, "y": "z"}]}]
    by_path = {f["path"]: f for f in summarize(docs)["fields"]}
    # ``items`` itself is an array.
    assert dict(by_path["items"]["types"]).get("array") == 1
    # Nested dict fields are walked.
    assert by_path["items.x"]["count"] == 2
    assert by_path["items.y"]["count"] == 1


def test_presence_reflects_optional_fields() -> None:
    docs = [{"a": 1}, {}, {"a": 2}, {}]
    by_path = {f["path"]: f for f in summarize(docs)["fields"]}
    assert by_path["a"]["count"] == 2
    assert abs(by_path["a"]["presence"] - 0.5) < 1e-9


def test_unhashable_values_skipped_from_top_values() -> None:
    docs = [{"x": [1, 2, 3]}, {"x": [1, 2, 3]}]
    by_path = {f["path"]: f for f in summarize(docs)["fields"]}
    # ``x`` is an array; top_values should be empty (skipped because
    # scalar_share is 0; arrays don't count).
    assert by_path["x"]["top_values"] == []


def test_datetime_typed_correctly() -> None:
    docs = [{"ts": dt.datetime(2026, 1, 1, tzinfo=dt.timezone.utc)}]
    by_path = {f["path"]: f for f in summarize(docs)["fields"]}
    assert dict(by_path["ts"]["types"]).get("datetime") == 1


def test_bool_distinct_from_int() -> None:
    docs = [{"flag": True}, {"flag": False}, {"flag": 1}]
    by_path = {f["path"]: f for f in summarize(docs)["fields"]}
    types = dict(by_path["flag"]["types"])
    assert types.get("bool") == 2
    assert types.get("int") == 1
