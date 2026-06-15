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


def test_bit_on_missing_field_treats_as_zero() -> None:
    assert apply_update({}, {"$bit": {"f": {"or": 0b101}}}) == {"f": 0b101}


def test_bit_on_non_int_raises() -> None:
    with pytest.raises(UpdateError):
        apply_update({"f": "abc"}, {"$bit": {"f": {"or": 1}}})
