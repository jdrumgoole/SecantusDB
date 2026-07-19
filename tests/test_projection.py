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


def test_elem_match_non_document_argument_raises() -> None:
    # mongod: the $elemMatch projection argument must be an object (Location31274).
    src = {"_id": 1, "arr": [1, 2, 3]}
    for arg in (5, "x", [1]):
        with pytest.raises(ProjectionError) as exc:
            apply_projection(src, {"arr": {"$elemMatch": arg}})
        assert exc.value.code == 31274, arg


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


def test_slice_argument_validation() -> None:
    # mongod: a non-number scalar / empty array / <2 or >3-element array is 28667;
    # a 2/3-element array that isn't [skip, positive-limit] is 28724.
    doc = {"_id": 1, "a": [1, 2, 3, 4, 5]}
    for sl, code in [
        ("x", 28667),
        (True, 28667),
        ([], 28667),
        ([1, -2], 28724),
        ([1, 2, 3], 28724),
        (["x", 2], 28724),
    ]:
        with pytest.raises(ProjectionError) as exc:
            apply_projection(dict(doc), {"a": {"$slice": sl}})
        assert exc.value.code == code, sl
    # Valid forms still slice: a number, and [skip, positive-limit] (skip may be <0).
    assert apply_projection(dict(doc), {"a": {"$slice": 2}})["a"] == [1, 2]
    assert apply_projection(dict(doc), {"a": {"$slice": -2}})["a"] == [4, 5]
    assert apply_projection(dict(doc), {"a": {"$slice": [1, 2]}})["a"] == [2, 3]
    assert apply_projection(dict(doc), {"a": {"$slice": [-3, 2]}})["a"] == [3, 4]


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


def test_positional_dotted_field() -> None:
    doc = {"_id": 1, "items": [{"k": "a", "n": 1}, {"k": "b", "n": 2}, {"k": "c", "n": 3}]}
    out = apply_projection(doc, {"items.$": 1}, {"items.k": "b"})
    assert out == {"_id": 1, "items": [{"k": "b", "n": 2}]}


def test_positional_scalar_range() -> None:
    doc = {"_id": 4, "nums": [1, 5, 10, 15]}
    out = apply_projection(doc, {"nums.$": 1}, {"nums": {"$gte": 10}})
    assert out == {"_id": 4, "nums": [10]}


def test_positional_elemmatch_query() -> None:
    doc = {"_id": 1, "items": [{"k": "a", "n": 1}, {"k": "c", "n": 3}]}
    out = apply_projection(doc, {"items.$": 1}, {"items": {"$elemMatch": {"n": {"$gt": 2}}}})
    assert out == {"_id": 1, "items": [{"k": "c", "n": 3}]}


def test_positional_first_of_many_plus_field() -> None:
    doc = {"_id": 2, "a": 7, "items": [{"k": "b", "n": 5}, {"k": "b", "n": 6}]}
    out = apply_projection(doc, {"_id": 0, "a": 1, "items.$": 1}, {"items.k": "b"})
    assert out == {"a": 7, "items": [{"k": "b", "n": 5}]}


def test_positional_errors() -> None:
    doc = {"_id": 1, "items": [{"k": "b"}]}
    # No query clause on the positional array.
    with pytest.raises(ProjectionError) as e1:
        apply_projection(doc, {"items.$": 1}, {"a": 1})
    assert e1.value.code == 51246
    # More than one positional.
    with pytest.raises(ProjectionError) as e2:
        apply_projection(doc, {"items.$": 1, "nums.$": 1}, {"items.k": "b", "nums": 1})
    assert e2.value.code == 31276
    # Positional with exclusion.
    with pytest.raises(ProjectionError) as e3:
        apply_projection(doc, {"items.$": 0}, {"items.k": "b"})
    assert e3.value.code == 31395


def test_validate_projection_parse_time() -> None:
    from secantus.projection import validate_projection

    # Validates even with no documents (mongod validates at parse time).
    with pytest.raises(ProjectionError) as e:
        validate_projection({"items.$": 1, "nums.$": 1}, {"items.k": "b"})
    assert e.value.code == 31276
    # A valid positional projection validates clean.
    validate_projection({"items.$": 1}, {"items.k": "b"})


def test_meta_unknown_arg_errors_17308() -> None:
    doc = {"_id": 1, "a": 1}
    with pytest.raises(ProjectionError) as e:
        apply_projection(doc, {"score": {"$meta": "bogus"}})
    assert e.value.code == 17308
    assert e.value.code_name == "Location17308"
    assert str(e.value) == "Unsupported argument to $meta: bogus"


def test_meta_textscore_without_text_errors_40218() -> None:
    doc = {"_id": 1, "a": 1}
    with pytest.raises(ProjectionError) as e:
        apply_projection(doc, {"score": {"$meta": "textScore"}}, {"a": 1})
    assert e.value.code == 40218
    assert e.value.code_name == "Location40218"
    assert str(e.value) == "query requires text score metadata, but it is not available"


def test_meta_textscore_with_text_query_omits_field() -> None:
    doc = {"_id": 1, "a": 1}
    out = apply_projection(doc, {"score": {"$meta": "textScore"}}, {"$text": {"$search": "x"}})
    # $meta field is omitted (not computed); inclusion projection keeps only _id.
    assert out == {"_id": 1}


def test_meta_textscore_with_nested_text_query() -> None:
    doc = {"_id": 1, "a": 1}
    out = apply_projection(
        doc,
        {"score": {"$meta": "textScore"}},
        {"$and": [{"a": 1}, {"$text": {"$search": "x"}}]},
    )
    assert out == {"_id": 1}


def test_meta_recognized_unsupported_arg_omits_field() -> None:
    doc = {"_id": 1, "a": 1, "b": 2}
    for arg in ("indexKey", "recordId", "sortKey"):
        out = apply_projection(doc, {"m": {"$meta": arg}})
        assert out == {"_id": 1}


def test_meta_alongside_inclusion_field() -> None:
    doc = {"_id": 1, "a": 1, "b": 2}
    out = apply_projection(doc, {"a": 1, "score": {"$meta": "indexKey"}})
    # Inclusion of `a`; the $meta field is omitted.
    assert out == {"_id": 1, "a": 1}


def test_meta_excludes_id() -> None:
    doc = {"_id": 1, "a": 1}
    out = apply_projection(doc, {"_id": 0, "score": {"$meta": "recordId"}})
    assert out == {}


def test_validate_meta_projection_parse_time() -> None:
    from secantus.projection import validate_meta_projection

    with pytest.raises(ProjectionError) as e:
        validate_meta_projection({"s": {"$meta": "nope"}}, None)
    assert e.value.code == 17308
    with pytest.raises(ProjectionError) as e2:
        validate_meta_projection({"s": {"$meta": "textScore"}}, {"a": 1})
    assert e2.value.code == 40218
    # Recognized-but-unsupported validates clean.
    validate_meta_projection({"s": {"$meta": "indexKey"}}, None)
