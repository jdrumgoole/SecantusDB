from __future__ import annotations

import pytest
from bson import Int64

from secantus.update import UpdateError, apply_update


def test_inc_preserves_int64_type() -> None:
    out = apply_update({"a": Int64(5)}, {"$inc": {"a": 3}})
    assert out["a"] == 8
    assert isinstance(out["a"], Int64)
    # A pure int32 field stays int32 (no spurious widening).
    out32 = apply_update({"a": 5}, {"$inc": {"a": 3}})
    assert out32["a"] == 8 and not isinstance(out32["a"], Int64)


def test_mul_preserves_int64_type() -> None:
    out = apply_update({"a": Int64(4)}, {"$mul": {"a": 2}})
    assert out["a"] == 8 and isinstance(out["a"], Int64)


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


def test_inc_absent_field_treated_as_zero() -> None:
    # A *missing* field is treated as 0 and the delta applied (mongod parity).
    assert apply_update({}, {"$inc": {"n": 5}}) == {"n": 5}
    assert apply_update({"other": 1}, {"$inc": {"n": 3}}) == {"other": 1, "n": 3}


def test_mul_absent_field_treated_as_zero() -> None:
    assert apply_update({}, {"$mul": {"n": 5}}) == {"n": 0}


def test_inc_explicit_null_field_errors_typemismatch() -> None:
    # A field present with an explicit null is a TypeMismatch (code 14) —
    # mongod refuses to coerce a present non-numeric value to 0.
    with pytest.raises(UpdateError) as excinfo:
        apply_update({"n": None}, {"$inc": {"n": 5}})
    assert excinfo.value.code == 14


def test_mul_explicit_null_field_errors_typemismatch() -> None:
    with pytest.raises(UpdateError) as excinfo:
        apply_update({"n": None}, {"$mul": {"n": 5}})
    assert excinfo.value.code == 14


def test_push_creates_array() -> None:
    assert apply_update({}, {"$push": {"tags": "x"}}) == {"tags": ["x"]}
    assert apply_update({"tags": ["x"]}, {"$push": {"tags": "y"}}) == {"tags": ["x", "y"]}


def test_pull_removes_matching() -> None:
    out = apply_update({"a": [1, 2, 3, 2]}, {"$pull": {"a": 2}})
    assert out == {"a": [1, 3]}


def test_pull_predicate_criterion() -> None:
    # An operator-only criterion is an element-value predicate.
    assert apply_update({"a": [1, 5, 10, 15]}, {"$pull": {"a": {"$gte": 10}}}) == {"a": [1, 5]}
    assert apply_update({"a": [1, 2, 3, 4]}, {"$pull": {"a": {"$in": [2, 4]}}}) == {"a": [1, 3]}


def test_pull_subdocument_match() -> None:
    docs = [{"x": 1, "y": "a"}, {"x": 5, "y": "b"}, {"x": 9, "y": "c"}]
    # A field-doc criterion matches the element as a sub-document.
    assert apply_update({"a": docs}, {"$pull": {"a": {"x": {"$gte": 5}}}}) == {
        "a": [{"x": 1, "y": "a"}]
    }
    assert apply_update({"a": docs}, {"$pull": {"a": {"y": "b"}}}) == {
        "a": [{"x": 1, "y": "a"}, {"x": 9, "y": "c"}]
    }


def test_pull_query_equality_types() -> None:
    # query eq: bool is type-distinct from int, but 1 == 1.0 numerically.
    assert apply_update({"a": [1, True, 2]}, {"$pull": {"a": 1}}) == {"a": [True, 2]}
    assert apply_update({"a": [1, 1.0, 2]}, {"$pull": {"a": 1}}) == {"a": [2]}


def test_pull_pullall_on_non_array_raises() -> None:
    # mongod: $pull / $pullAll on a present but non-array field (scalar or null)
    # is code 2; a missing field is a silent no-op.
    for upd in ({"$pull": {"n": 1}}, {"$pullAll": {"n": [1]}}):
        with pytest.raises(UpdateError) as exc:
            apply_update({"n": 5}, upd)
        assert exc.value.code == 2, upd
        with pytest.raises(UpdateError) as exc:
            apply_update({"n": None}, upd)
        assert exc.value.code == 2, upd
    # Missing field: no-op (document unchanged).
    assert apply_update({"other": 1}, {"$pull": {"nope": 1}}) == {"other": 1}
    assert apply_update({"other": 1}, {"$pullAll": {"nope": [1]}}) == {"other": 1}


def test_pullall_removes_listed_values() -> None:
    assert apply_update({"a": [1, 2, 3, 2, 1]}, {"$pullAll": {"a": [1, 2]}}) == {"a": [3]}
    assert apply_update({"a": [1, 2, 3]}, {"$pullAll": {"a": [9]}}) == {"a": [1, 2, 3]}
    with pytest.raises(UpdateError):
        apply_update({"a": [1]}, {"$pullAll": {"a": 5}})


def test_push_sort_modifier() -> None:
    # Whole-element sort, ascending / descending.
    assert apply_update({"a": [3, 1]}, {"$push": {"a": {"$each": [2], "$sort": 1}}}) == {
        "a": [1, 2, 3]
    }
    assert apply_update({"a": [1, 3]}, {"$push": {"a": {"$each": [2], "$sort": -1}}}) == {
        "a": [3, 2, 1]
    }
    # Sort by sub-field, then slice.
    out = apply_update(
        {"a": [{"s": 3}, {"s": 1}]},
        {"$push": {"a": {"$each": [{"s": 2}], "$sort": {"s": 1}}}},
    )
    assert out == {"a": [{"s": 1}, {"s": 2}, {"s": 3}]}
    # A whole-double scalar / direction is accepted (coerces like ±1).
    assert apply_update({"a": [3, 1]}, {"$push": {"a": {"$each": [2], "$sort": 1.0}}}) == {
        "a": [1, 2, 3]
    }
    assert apply_update(
        {"a": [{"s": 3}]}, {"$push": {"a": {"$each": [{"s": 1}], "$sort": {"s": 1.0}}}}
    ) == {"a": [{"s": 1}, {"s": 3}]}


def test_push_sort_invalid_spec_raises() -> None:
    # mongod: a numeric whole-element $sort must be exactly ±1 (else code 2), a
    # document direction must be ±1 (else code 2), and a non-numeric spec
    # (string/bool/array) is code 2 with the "invalid" message.
    for spec in (2, -2, 1.5):
        with pytest.raises(UpdateError) as exc:
            apply_update({"a": [3, 1]}, {"$push": {"a": {"$each": [2], "$sort": spec}}})
        assert exc.value.code == 2, spec
    for spec in ({"s": 2}, {"s": True}, {"s": "x"}):
        with pytest.raises(UpdateError) as exc:
            apply_update({"a": [{"s": 1}]}, {"$push": {"a": {"$each": [{"s": 2}], "$sort": spec}}})
        assert exc.value.code == 2, spec
    for spec in ("x", True, [1]):
        with pytest.raises(UpdateError) as exc:
            apply_update({"a": [3, 1]}, {"$push": {"a": {"$each": [2], "$sort": spec}}})
        assert exc.value.code == 2, spec


def test_current_date_bool_and_type_validation() -> None:
    import datetime

    import bson

    # A boolean (true OR false) sets the current Date.
    for flag in (True, False):
        out = apply_update({"_id": 1}, {"$currentDate": {"d": flag}})
        assert isinstance(out["d"], datetime.datetime), flag
    # {$type: "date"} / {$type: "timestamp"} set the right BSON type.
    assert isinstance(
        apply_update({"_id": 1}, {"$currentDate": {"d": {"$type": "date"}}})["d"],
        datetime.datetime,
    )
    assert isinstance(
        apply_update({"_id": 1}, {"$currentDate": {"d": {"$type": "timestamp"}}})["d"],
        bson.Timestamp,
    )
    # A non-bool scalar and a bad/missing $type are code 2.
    for opt in (5, "x", [1]):
        with pytest.raises(UpdateError) as exc:
            apply_update({"_id": 1}, {"$currentDate": {"d": opt}})
        assert exc.value.code == 2, opt
    for opt in ({"$type": "bogus"}, {"$type": 5}, {}):
        with pytest.raises(UpdateError) as exc:
            apply_update({"_id": 1}, {"$currentDate": {"d": opt}})
        assert exc.value.code == 2, opt


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


def test_min_max_cross_type_bson_order() -> None:
    # mongod compares by BSON canonical-type order (null < number < string <
    # objectId < bool < date). No Python TypeError leak on cross-type.
    from bson import ObjectId

    d = __import__("datetime").datetime(2026, 1, 1)
    oid = ObjectId("507f1f77bcf86cd799439011")
    assert apply_update({"a": 5}, {"$max": {"a": "str"}}) == {"a": "str"}  # string > number
    assert apply_update({"a": 5}, {"$max": {"a": d}}) == {"a": d}  # date > number
    assert apply_update({"a": d}, {"$max": {"a": "str"}}) == {"a": d}  # date > string
    assert apply_update({"a": oid}, {"$max": {"a": 5}}) == {"a": oid}  # objectId > number
    assert apply_update({"a": 5}, {"$min": {"a": "str"}}) == {"a": 5}  # number < string


def test_min_max_missing_vs_explicit_null() -> None:
    # A MISSING field is set unconditionally; an explicit null is a real value
    # (BSON rank 2, below numbers), compared by order — not "no current".
    assert apply_update({}, {"$max": {"a": 5}}) == {"a": 5}  # missing -> set
    assert apply_update({"a": None}, {"$max": {"a": 5}}) == {"a": 5}  # 5 > null -> set
    assert apply_update({"a": None}, {"$min": {"a": 5}}) == {"a": None}  # null < 5 -> keep null


def test_apply_does_not_mutate_input() -> None:
    src = {"a": 1, "nested": {"b": [1, 2]}}
    apply_update(src, {"$set": {"nested.b.0": 99}, "$inc": {"a": 1}})
    assert src == {"a": 1, "nested": {"b": [1, 2]}}


def test_pipeline_update_with_set_stage() -> None:
    out = apply_update(
        {"_id": 1, "a": 1, "b": 2},
        [{"$set": {"sum": {"$add": ["$a", "$b"]}}}],
    )
    assert out == {"_id": 1, "a": 1, "b": 2, "sum": 3}


def test_pipeline_update_chains_stages() -> None:
    out = apply_update(
        {"_id": 1, "x": 10},
        [
            {"$set": {"doubled": {"$multiply": ["$x", 2]}}},
            {"$unset": "x"},
        ],
    )
    assert out == {"_id": 1, "doubled": 20}


def test_pipeline_update_rejects_id_change() -> None:
    with pytest.raises(UpdateError):
        apply_update({"_id": 1, "x": 1}, [{"$set": {"_id": 99}}])


def test_pipeline_update_rejects_disallowed_stage() -> None:
    with pytest.raises(UpdateError):
        apply_update({"_id": 1, "x": 1}, [{"$match": {"x": 1}}])


def test_current_date_default_sets_datetime() -> None:
    import datetime as dt

    out = apply_update({"_id": 1}, {"$currentDate": {"updated": True}})
    assert isinstance(out["updated"], dt.datetime)


def test_current_date_with_type_date() -> None:
    import datetime as dt

    out = apply_update({"_id": 1}, {"$currentDate": {"updated": {"$type": "date"}}})
    assert isinstance(out["updated"], dt.datetime)


def test_current_date_with_type_timestamp() -> None:
    from bson import Timestamp

    out = apply_update({"_id": 1}, {"$currentDate": {"ts": {"$type": "timestamp"}}})
    assert isinstance(out["ts"], Timestamp)


def test_set_on_insert_skipped_for_existing_doc() -> None:
    out = apply_update({"_id": 1, "n": 5}, {"$setOnInsert": {"created": True}}, is_upsert=False)
    assert "created" not in out


def test_set_on_insert_applied_during_upsert() -> None:
    out = apply_update({}, {"$setOnInsert": {"created": True}}, is_upsert=True)
    assert out["created"] is True


def test_positional_all_set_on_every_element() -> None:
    out = apply_update(
        {"_id": 1, "items": [{"qty": 1}, {"qty": 2}, {"qty": 3}]},
        {"$set": {"items.$[].qty": 0}},
    )
    assert [e["qty"] for e in out["items"]] == [0, 0, 0]


def test_positional_filtered_set_only_matching() -> None:
    out = apply_update(
        {"_id": 1, "items": [{"qty": 1, "tag": "a"}, {"qty": 5, "tag": "b"}, {"qty": 9}]},
        {"$set": {"items.$[hi].tag": "BIG"}},
        array_filters=[{"hi.qty": {"$gte": 5}}],
    )
    assert out["items"][0] == {"qty": 1, "tag": "a"}
    assert out["items"][1] == {"qty": 5, "tag": "BIG"}
    assert out["items"][2] == {"qty": 9, "tag": "BIG"}


def test_positional_inc_on_filtered_elements() -> None:
    out = apply_update(
        {"_id": 1, "scores": [10, 20, 30, 40]},
        {"$inc": {"scores.$[gt20]": 100}},
        array_filters=[{"gt20": {"$gt": 20}}],
    )
    assert out["scores"] == [10, 20, 130, 140]


def test_positional_unset_clears_all_elements() -> None:
    out = apply_update(
        {"_id": 1, "items": [{"qty": 1, "tag": "a"}, {"qty": 2, "tag": "b"}]},
        {"$unset": {"items.$[].tag": ""}},
    )
    assert out["items"] == [{"qty": 1}, {"qty": 2}]


def test_positional_with_unknown_filter_name_raises() -> None:
    with pytest.raises(UpdateError):
        apply_update(
            {"items": [{"x": 1}]},
            {"$set": {"items.$[unknown].x": 9}},
            array_filters=[{"other.x": 1}],
        )


def test_array_filters_validation() -> None:
    # mongod: a non-object filter (14), an empty filter (9), a bad identifier
    # (2), a duplicate identifier (9), and an identifier not used by any
    # `$[id]` path (9) are all rejected before the update is applied.
    doc = {"a": [{"g": 1}, {"g": 5}]}
    upd = {"$set": {"a.$[x].g": 9}}
    for af, code in [
        (["x"], 14),  # non-object filter
        ([{}], 9),  # empty filter
        ([{"1x": {"$gt": 0}}], 2),  # identifier starts with a digit
        ([{"X": {"$gt": 0}}], 2),  # identifier starts uppercase
        ([{"x": {"$gt": 0}}, {"x": {"$lt": 9}}], 9),  # duplicate identifier
        ([{"x": {"$gt": 0}}, {"y": {"$gt": 0}}], 9),  # 'y' unused
        ([{"x": {"$gt": 0}, "y": {"$gt": 0}}], 9),  # two identifiers in one filter
        ([{"$and": [{"x": {"$gt": 0}}, {"y": {"$gt": 0}}]}], 9),  # two, nested
        ([{"$expr": {"$gt": ["$g", 0]}}], 224),  # $expr, no identifier
    ]:
        with pytest.raises(UpdateError) as exc:
            apply_update(dict(doc), upd, array_filters=af)
        assert exc.value.code == code, af
    # Valid: an identifier used by the update path (a dotted filter key on the
    # element's sub-field). All elements match here; a stricter filter matches one.
    assert apply_update(dict(doc), upd, array_filters=[{"x.g": {"$gt": 0}}]) == {
        "a": [{"g": 9}, {"g": 9}]
    }
    assert apply_update(dict(doc), upd, array_filters=[{"x.g": {"$gt": 3}}]) == {
        "a": [{"g": 1}, {"g": 9}]
    }
    # A single identifier nested inside $and / $or resolves and applies.
    assert apply_update(
        {"a": [{"g": 1, "h": 1}, {"g": 5, "h": 9}]},
        upd,
        array_filters=[{"$and": [{"x.g": {"$gt": 3}}, {"x.h": {"$gt": 0}}]}],
    ) == {"a": [{"g": 1, "h": 1}, {"g": 9, "h": 9}]}
    assert apply_update(
        {"a": [{"g": 1}, {"g": 5}]},
        upd,
        array_filters=[{"$or": [{"x.g": {"$gt": 3}}, {"x.g": {"$lt": 0}}]}],
    ) == {"a": [{"g": 1}, {"g": 9}]}


def test_positional_dollar_sets_first_match() -> None:
    out = apply_update(
        {"_id": 1, "items": [{"qty": 1}, {"qty": 5}, {"qty": 5}]},
        {"$set": {"items.$.tag": "BIG"}},
        positional_matches={"items": 1},
    )
    assert out["items"][1]["tag"] == "BIG"
    assert "tag" not in out["items"][0]
    assert "tag" not in out["items"][2]


def test_positional_dollar_without_match_raises() -> None:
    with pytest.raises(UpdateError):
        apply_update(
            {"_id": 1, "items": [{"qty": 1}]},
            {"$set": {"items.$.tag": "X"}},
        )


def test_find_positional_matches_dotted_path() -> None:
    from secantus.update import find_positional_matches

    doc = {"_id": 1, "items": [{"sku": "a", "qty": 1}, {"sku": "b", "qty": 5}]}
    assert find_positional_matches(doc, {"items.qty": {"$gte": 5}}) == {"items": 1}


def test_find_positional_matches_no_array_returns_empty() -> None:
    from secantus.update import find_positional_matches

    assert find_positional_matches({"x": 1}, {"x": 1}) == {}


def test_bit_and_or_xor() -> None:
    assert apply_update({"f": 0b1100}, {"$bit": {"f": {"and": 0b1010}}}) == {"f": 0b1000}
    assert apply_update({"f": 0b1100}, {"$bit": {"f": {"or": 0b0011}}}) == {"f": 0b1111}
    assert apply_update({"f": 0b1100}, {"$bit": {"f": {"xor": 0b1010}}}) == {"f": 0b0110}


def test_bit_multiple_ops_applied_in_order() -> None:
    # mongod applies every listed op in order: (v & 0b1010) | 0b0001.
    assert apply_update({"f": 0b1100}, {"$bit": {"f": {"and": 0b1010, "or": 0b0001}}}) == {
        "f": 0b1001
    }
    # xor after or.
    assert apply_update({"f": 0b1000}, {"$bit": {"f": {"or": 0b0001, "xor": 0b1001}}}) == {"f": 0}
    # An empty $bit doc is rejected.
    with pytest.raises(UpdateError):
        apply_update({"f": 1}, {"$bit": {"f": {}}})


def test_bit_on_missing_field_treats_as_zero() -> None:
    assert apply_update({}, {"$bit": {"f": {"or": 0b101}}}) == {"f": 0b101}


def test_bit_on_non_int_raises() -> None:
    with pytest.raises(UpdateError):
        apply_update({"f": "abc"}, {"$bit": {"f": {"or": 1}}})


def test_inc_mul_reject_non_numeric_operand() -> None:
    """$inc / $mul by a non-number raise code 14 with mongod's message
    (probed 7.0.12). bool is not a number here (Python's bool is an int, so
    5 + True would otherwise compute); string / null also error instead of
    raising a raw ValueError/TypeError from the arithmetic. Valid numeric
    operands still apply."""
    for op, verb in [("$inc", "increment"), ("$mul", "multiply")]:
        for operand in (True, False, "x", None):
            with pytest.raises(UpdateError) as exc:
                apply_update({"n": 5}, {op: {"n": operand}})
            assert exc.value.code == 14
            assert f"Cannot {verb} with non-numeric argument" in str(exc.value)
    # Valid numeric operands still apply.
    assert apply_update({"n": 5}, {"$inc": {"n": 3}}) == {"n": 8}
    assert apply_update({"n": 5}, {"$mul": {"n": 2.5}}) == {"n": 12.5}


def test_pop_position_slice_bit_reject_bool() -> None:
    """The update-operator bool-as-int cluster (probed vs mongod 7.0.12): a bool
    argument to $pop / $push $position / $push $slice / $bit is a parse error,
    not silently treated as 1. $pop's codes: bool / non-±1 both code 9;
    $position / $slice / $bit code 2."""
    # $pop: bool and non-±1 both error (code 9).
    with pytest.raises(UpdateError) as e:
        apply_update({"a": [1, 2, 3]}, {"$pop": {"a": True}})
    assert e.value.code == 9
    with pytest.raises(UpdateError) as e:
        apply_update({"a": [1, 2, 3]}, {"$pop": {"a": 2}})
    assert e.value.code == 9
    # $position / $slice / $bit bool -> code 2.
    for upd in (
        {"$push": {"a": {"$each": [9], "$position": True}}},
        {"$push": {"a": {"$each": [], "$slice": True}}},
        {"$bit": {"a": {"and": True}}},
    ):
        with pytest.raises(UpdateError) as e:
            apply_update({"a": [1, 2, 3]}, upd)
        assert e.value.code == 2, upd
    # Valid arguments still apply.
    assert apply_update({"a": [1, 2, 3]}, {"$pop": {"a": 1}}) == {"a": [1, 2]}
    assert apply_update({"a": [1, 2]}, {"$push": {"a": {"$each": [9], "$position": 1}}}) == {
        "a": [1, 9, 2]
    }


def test_rename_validation_and_no_corruption() -> None:
    """$rename validates its spec like mongod instead of silently corrupting the
    document or leaking a raw exception. mongod 7.0.12-verified."""
    import copy

    base = {"_id": 1, "a": 5, "arr": [1, 2, 3], "b": 9}
    # Valid renames still apply.
    assert apply_update(copy.deepcopy(base), {"$rename": {"a": "z"}})["z"] == 5
    assert apply_update(copy.deepcopy(base), {"$rename": {"a": "x.y"}})["x"] == {"y": 5}
    assert "a" in apply_update(
        copy.deepcopy(base), {"$rename": {"gone": "z"}}
    )  # missing src: no-op
    # Invalid specs raise (were silent corruption / an AttributeError leak).
    for upd, code in [
        ({"$rename": {"a": "a"}}, 2),  # same field
        ({"$rename": {"arr.0": "x"}}, 2),  # source is an array element (was corruption)
        ({"$rename": {"a": "arr.0"}}, 2),  # dest is an array element
        ({"$rename": {"a": "a.b"}}, 2),  # same path
        ({"$rename": {"a": ""}}, 56),  # empty target
        ({"$rename": {"a": 5}}, 2),  # non-string target (was AttributeError leak)
        ({"$rename": {"a": True}}, 2),
    ]:
        with pytest.raises(UpdateError) as exc:
            apply_update(copy.deepcopy(base), upd)
        assert exc.value.code == code, upd
    # The document is untouched when the update is rejected.
    d = copy.deepcopy(base)
    with pytest.raises(UpdateError):
        apply_update(d, {"$rename": {"arr.0": "x"}})
    assert d["arr"] == [1, 2, 3]
