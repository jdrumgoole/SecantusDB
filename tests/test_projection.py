from __future__ import annotations

import pytest

from secantus.projection import ProjectionError, apply_projection


def test_no_spec_returns_copy() -> None:
    src = {"_id": 1, "a": 1, "b": 2}
    out = apply_projection(src, None)
    assert out == src
    assert out is not src


def test_inclusion_keeps_id_by_default() -> None:
    out = apply_projection({"_id": 1, "a": 1, "b": 2}, {"a": 1})
    assert out == {"_id": 1, "a": 1}


def test_inclusion_can_exclude_id() -> None:
    out = apply_projection({"_id": 1, "a": 1, "b": 2}, {"_id": 0, "a": 1})
    assert out == {"a": 1}


def test_exclusion_keeps_everything_else() -> None:
    out = apply_projection({"_id": 1, "a": 1, "b": 2, "c": 3}, {"b": 0})
    assert out == {"_id": 1, "a": 1, "c": 3}


def test_only_id_zero_drops_id() -> None:
    out = apply_projection({"_id": 1, "a": 1, "b": 2}, {"_id": 0})
    assert out == {"a": 1, "b": 2}


def test_dotted_inclusion_preserves_nested_structure() -> None:
    src = {"_id": 1, "a": {"b": 1, "c": 2}, "x": 9}
    out = apply_projection(src, {"a.b": 1})
    assert out == {"_id": 1, "a": {"b": 1}}


def test_dotted_exclusion_drops_nested_only() -> None:
    src = {"_id": 1, "a": {"b": 1, "c": 2}, "x": 9}
    out = apply_projection(src, {"a.b": 0})
    assert out == {"_id": 1, "a": {"c": 2}, "x": 9}


def test_missing_field_in_inclusion_omitted() -> None:
    out = apply_projection({"_id": 1, "a": 1}, {"missing": 1})
    assert out == {"_id": 1}


def test_mixed_inclusion_exclusion_rejected() -> None:
    with pytest.raises(ProjectionError):
        apply_projection({"a": 1, "b": 2}, {"a": 1, "b": 0})


def test_does_not_mutate_input() -> None:
    src = {"_id": 1, "a": {"b": 1}}
    apply_projection(src, {"a.b": 0})
    assert src == {"_id": 1, "a": {"b": 1}}


def test_elem_match_returns_first_matching_subdoc() -> None:
    src = {"_id": 1, "items": [{"qty": 1}, {"qty": 5}, {"qty": 10}]}
    out = apply_projection(src, {"items": {"$elemMatch": {"qty": {"$gte": 5}}}})
    assert out == {"_id": 1, "items": [{"qty": 5}]}


def test_elem_match_omits_field_when_no_match() -> None:
    src = {"_id": 1, "items": [{"qty": 1}, {"qty": 2}]}
    out = apply_projection(src, {"items": {"$elemMatch": {"qty": {"$gte": 100}}}})
    assert out == {"_id": 1}


def test_elem_match_combined_with_other_inclusions() -> None:
    src = {"_id": 1, "name": "a", "items": [{"qty": 1}, {"qty": 5}]}
    out = apply_projection(
        src,
        {"_id": 0, "name": 1, "items": {"$elemMatch": {"qty": {"$gte": 5}}}},
    )
    assert out == {"name": "a", "items": [{"qty": 5}]}
