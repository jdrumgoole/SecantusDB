from __future__ import annotations

import pytest

from fongodb.aggregate import AggregateError, apply_pipeline


def test_match_filters_docs() -> None:
    docs = [{"x": 1}, {"x": 2}, {"x": 3}]
    out = apply_pipeline(docs, [{"$match": {"x": {"$gte": 2}}}])
    assert [d["x"] for d in out] == [2, 3]


def test_sort_pipeline_stage() -> None:
    docs = [{"x": 3}, {"x": 1}, {"x": 2}]
    out = apply_pipeline(docs, [{"$sort": {"x": 1}}])
    assert [d["x"] for d in out] == [1, 2, 3]


def test_project_inclusion_keeps_id_by_default() -> None:
    docs = [{"_id": 1, "a": 1, "b": 2}]
    out = apply_pipeline(docs, [{"$project": {"a": 1}}])
    assert out == [{"_id": 1, "a": 1}]


def test_project_with_computed_field() -> None:
    docs = [{"x": 3, "y": 4}]
    out = apply_pipeline(docs, [{"$project": {"_id": 0, "sum": {"$add": ["$x", "$y"]}}}])
    assert out == [{"sum": 7}]


def test_project_rename_via_field_path() -> None:
    docs = [{"_id": 1, "a": "hello"}]
    out = apply_pipeline(docs, [{"$project": {"_id": 0, "renamed": "$a"}}])
    assert out == [{"renamed": "hello"}]


def test_add_fields() -> None:
    docs = [{"x": 1}]
    out = apply_pipeline(docs, [{"$addFields": {"y": 2, "doubled": {"$multiply": ["$x", 2]}}}])
    assert out == [{"x": 1, "y": 2, "doubled": 2}]


def test_set_alias_for_add_fields() -> None:
    docs = [{"x": 1}]
    out = apply_pipeline(docs, [{"$set": {"y": 2}}])
    assert out == [{"x": 1, "y": 2}]


def test_unset_string_form() -> None:
    docs = [{"a": 1, "b": 2}]
    out = apply_pipeline(docs, [{"$unset": "b"}])
    assert out == [{"a": 1}]


def test_unset_list_form() -> None:
    docs = [{"a": 1, "b": 2, "c": 3}]
    out = apply_pipeline(docs, [{"$unset": ["a", "c"]}])
    assert out == [{"b": 2}]


def test_unwind_simple_path() -> None:
    docs = [{"_id": 1, "tags": ["a", "b", "c"]}]
    out = apply_pipeline(docs, [{"$unwind": "$tags"}])
    assert [d["tags"] for d in out] == ["a", "b", "c"]


def test_unwind_with_index() -> None:
    docs = [{"_id": 1, "tags": ["x", "y"]}]
    out = apply_pipeline(docs, [{"$unwind": {"path": "$tags", "includeArrayIndex": "i"}}])
    assert [(d["tags"], d["i"]) for d in out] == [("x", 0), ("y", 1)]


def test_unwind_empty_array_drops_doc_by_default() -> None:
    docs = [{"_id": 1, "tags": []}, {"_id": 2, "tags": ["x"]}]
    out = apply_pipeline(docs, [{"$unwind": "$tags"}])
    assert [d["_id"] for d in out] == [2]


def test_unwind_preserve_null() -> None:
    docs = [{"_id": 1, "tags": []}, {"_id": 2}, {"_id": 3, "tags": ["x"]}]
    out = apply_pipeline(docs, [{"$unwind": {"path": "$tags", "preserveNullAndEmptyArrays": True}}])
    assert sorted(d["_id"] for d in out) == [1, 2, 3]


def test_replace_root() -> None:
    docs = [{"_id": 1, "inner": {"a": 1, "b": 2}}]
    out = apply_pipeline(docs, [{"$replaceRoot": {"newRoot": "$inner"}}])
    assert out == [{"a": 1, "b": 2}]


def test_replace_with_alias() -> None:
    docs = [{"inner": {"a": 1}}]
    out = apply_pipeline(docs, [{"$replaceWith": "$inner"}])
    assert out == [{"a": 1}]


def test_group_sum_count() -> None:
    docs = [
        {"team": "a", "score": 10},
        {"team": "a", "score": 5},
        {"team": "b", "score": 7},
    ]
    out = apply_pipeline(
        docs,
        [{"$group": {"_id": "$team", "total": {"$sum": "$score"}, "n": {"$sum": 1}}}],
    )
    by_id = {d["_id"]: d for d in out}
    assert by_id["a"]["total"] == 15
    assert by_id["a"]["n"] == 2
    assert by_id["b"]["total"] == 7
    assert by_id["b"]["n"] == 1


def test_group_avg() -> None:
    docs = [{"x": 2}, {"x": 4}, {"x": 6}]
    out = apply_pipeline(docs, [{"$group": {"_id": None, "avg": {"$avg": "$x"}}}])
    assert out == [{"_id": None, "avg": 4.0}]


def test_group_min_max_first_last_push_addtoset() -> None:
    docs = [{"x": 3}, {"x": 1}, {"x": 2}, {"x": 1}]
    out = apply_pipeline(
        docs,
        [
            {
                "$group": {
                    "_id": None,
                    "min": {"$min": "$x"},
                    "max": {"$max": "$x"},
                    "first": {"$first": "$x"},
                    "last": {"$last": "$x"},
                    "all": {"$push": "$x"},
                    "set": {"$addToSet": "$x"},
                }
            }
        ],
    )
    [bucket] = out
    assert bucket["min"] == 1
    assert bucket["max"] == 3
    assert bucket["first"] == 3
    assert bucket["last"] == 1
    assert bucket["all"] == [3, 1, 2, 1]
    assert sorted(bucket["set"]) == [1, 2, 3]


def test_pipeline_chain_match_sort_project_limit() -> None:
    docs = [{"_id": i, "x": i, "name": f"n{i}"} for i in range(10)]
    out = apply_pipeline(
        docs,
        [
            {"$match": {"x": {"$gte": 5}}},
            {"$sort": {"x": -1}},
            {"$project": {"_id": 0, "x": 1}},
            {"$limit": 3},
        ],
    )
    assert out == [{"x": 9}, {"x": 8}, {"x": 7}]


def test_unsupported_stage_raises() -> None:
    with pytest.raises(AggregateError):
        apply_pipeline([{"x": 1}], [{"$bogusStage": {}}])


def test_project_mixed_inclusion_exclusion_rejected() -> None:
    with pytest.raises(AggregateError):
        apply_pipeline([{"a": 1, "b": 2}], [{"$project": {"a": 1, "b": 0}}])
