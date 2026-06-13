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


def test_id_only_truthy_spec_is_inclusion() -> None:
    """{"_id": 1} (and any non-zero value) is an inclusion projection:
    only _id survives. Oracle-pinned against real mongod."""
    doc = {"_id": 7, "a": 1, "b": 2}
    assert apply_projection(doc, {"_id": 1}) == {"_id": 7}
    assert apply_projection(doc, {"_id": True}) == {"_id": 7}
    # mongod treats None and "" as include, not drop.
    assert apply_projection(doc, {"_id": None}) == {"_id": 7}
    assert apply_projection(doc, {"_id": ""}) == {"_id": 7}


def test_id_only_falsy_spec_is_exclusion() -> None:
    doc = {"_id": 7, "a": 1, "b": 2}
    assert apply_projection(doc, {"_id": 0}) == {"a": 1, "b": 2}
    assert apply_projection(doc, {"_id": 0.0}) == {"a": 1, "b": 2}
    assert apply_projection(doc, {"_id": False}) == {"a": 1, "b": 2}


def test_slice_with_explicit_truthy_id_flips_to_inclusion() -> None:
    """$slice alone keeps the whole doc, but adding _id: 1 makes the
    projection inclusive: only _id plus the sliced field survive."""
    doc = {"_id": 7, "a": [1, 2, 3], "b": 9}
    assert apply_projection(doc, {"a": {"$slice": 2}, "_id": 1}) == {
        "_id": 7,
        "a": [1, 2],
    }
    # _id: 0 with slice-only stays exclusion: whole doc minus _id, sliced.
    assert apply_projection(doc, {"a": {"$slice": 2}, "_id": 0}) == {
        "a": [1, 2],
        "b": 9,
    }
    # No _id key at all: whole doc with the slice applied (unchanged).
    assert apply_projection(doc, {"a": {"$slice": 2}}) == {
        "_id": 7,
        "a": [1, 2],
        "b": 9,
    }


def test_inclusion_fans_over_arrays() -> None:
    """Dotted inclusion paths map over array elements: doc elements
    project (possibly to {}), scalar elements drop. Oracle-pinned."""
    doc = {"_id": 1, "a": [{"q": 1, "w": 2}, {"w": 3}, 7], "b": 9}
    assert apply_projection(doc, {"a.q": 1}) == {"_id": 1, "a": [{"q": 1}, {}]}
    deep = {"_id": 1, "a": [{"x": {"q": 1, "r": 2}}, {"x": 5}], "b": 9}
    assert apply_projection(deep, {"a.x.q": 1}) == {
        "_id": 1,
        "a": [{"x": {"q": 1}}, {}],
    }
    nested = {"_id": 1, "a": [[{"q": 1, "w": 2}], {"q": 5, "w": 6}]}
    assert apply_projection(nested, {"a.q": 1}) == {
        "_id": 1,
        "a": [[{"q": 1}], {"q": 5}],
    }
    merged = {"_id": 1, "a": [{"q": 1, "w": 2, "z": 3}], "b": 9}
    assert apply_projection(merged, {"a.q": 1, "a.w": 1}) == {
        "_id": 1,
        "a": [{"q": 1, "w": 2}],
    }


def test_exclusion_fans_over_arrays() -> None:
    """Dotted exclusion unsets per array element; scalar elements and
    unrelated fields survive untouched."""
    doc = {"_id": 1, "a": [{"q": 1, "w": 2}, {"w": 3}, 7], "b": 9}
    assert apply_projection(doc, {"a.q": 0}) == {
        "_id": 1,
        "a": [{"w": 2}, {"w": 3}, 7],
        "b": 9,
    }


def test_inclusion_keeps_dict_skeleton_for_missing_leaf() -> None:
    """A dotted inclusion path whose leaf is absent keeps the dict
    skeleton ({} at the deepest reachable doc); a non-doc prefix drops
    the field entirely. Numeric segments are field names, not indexes."""
    assert apply_projection({"_id": 1, "a": {"w": 2}, "b": 9}, {"a.q": 1}) == {
        "_id": 1,
        "a": {},
    }
    assert apply_projection({"_id": 1, "a": 5, "b": 9}, {"a.q": 1}) == {"_id": 1}
    assert apply_projection({"_id": 1, "a": [{"q": 1}], "b": 9}, {"a.0.q": 1}) == {
        "_id": 1,
        "a": [{}],
    }
