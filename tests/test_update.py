from __future__ import annotations

import pytest

from fongodb.update import UpdateError, apply_update


def test_set_top_level() -> None:
    out = apply_update({"a": 1}, {"$set": {"b": 2}})
    assert out == {"a": 1, "b": 2}


def test_set_creates_nested() -> None:
    out = apply_update({}, {"$set": {"a.b.c": 5}})
    assert out == {"a": {"b": {"c": 5}}}


def test_unset_removes_key() -> None:
    out = apply_update({"a": 1, "b": 2}, {"$unset": {"b": ""}})
    assert out == {"a": 1}


def test_inc_existing_and_missing() -> None:
    assert apply_update({"a": 1}, {"$inc": {"a": 2}}) == {"a": 3}
    assert apply_update({}, {"$inc": {"a": 5}}) == {"a": 5}


def test_push_creates_array() -> None:
    assert apply_update({}, {"$push": {"tags": "x"}}) == {"tags": ["x"]}
    assert apply_update({"tags": ["x"]}, {"$push": {"tags": "y"}}) == {"tags": ["x", "y"]}


def test_pull_removes_matching() -> None:
    out = apply_update({"a": [1, 2, 3, 2]}, {"$pull": {"a": 2}})
    assert out == {"a": [1, 3]}


def test_addtoset_dedupes() -> None:
    out = apply_update({"a": [1, 2]}, {"$addToSet": {"a": 2}})
    assert out == {"a": [1, 2]}
    out = apply_update({"a": [1, 2]}, {"$addToSet": {"a": 3}})
    assert out == {"a": [1, 2, 3]}


def test_replacement_preserves_id() -> None:
    out = apply_update({"_id": 1, "a": 1, "b": 2}, {"x": 99})
    assert out == {"_id": 1, "x": 99}


def test_replacement_rejects_id_change() -> None:
    with pytest.raises(UpdateError):
        apply_update({"_id": 1}, {"_id": 2, "x": 99})


def test_mixed_operators_and_fields_rejected() -> None:
    with pytest.raises(UpdateError):
        apply_update({}, {"$set": {"a": 1}, "b": 2})


def test_rename() -> None:
    out = apply_update({"a": 1}, {"$rename": {"a": "b"}})
    assert out == {"b": 1}


def test_min_max() -> None:
    assert apply_update({"a": 5}, {"$min": {"a": 3}}) == {"a": 3}
    assert apply_update({"a": 5}, {"$min": {"a": 9}}) == {"a": 5}
    assert apply_update({"a": 5}, {"$max": {"a": 9}}) == {"a": 9}
    assert apply_update({"a": 5}, {"$max": {"a": 3}}) == {"a": 5}


def test_apply_does_not_mutate_input() -> None:
    src = {"a": 1, "nested": {"b": [1, 2]}}
    apply_update(src, {"$set": {"nested.b.0": 99}, "$inc": {"a": 1}})
    assert src == {"a": 1, "nested": {"b": [1, 2]}}
