from __future__ import annotations

import pytest
from bson import Decimal128, ObjectId, Regex

from secantus.query import QueryError, matches


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


def test_regex_basic_anchored_match() -> None:
    assert matches({"name": "alice"}, {"name": {"$regex": "^ali"}})
    assert not matches({"name": "bob"}, {"name": {"$regex": "^ali"}})


def test_regex_case_insensitive_via_options() -> None:
    assert matches({"name": "ALICE"}, {"name": {"$regex": "alice", "$options": "i"}})


def test_regex_as_bson_regex_value() -> None:
    assert matches({"name": "ALICE"}, {"name": Regex("alice", "i")})
    assert not matches({"name": "bob"}, {"name": Regex("alice", "i")})


def test_regex_matches_array_element() -> None:
    assert matches({"tags": ["foo", "bar"]}, {"tags": {"$regex": "^ba"}})


def test_type_by_alias() -> None:
    assert matches({"a": "hi"}, {"a": {"$type": "string"}})
    assert not matches({"a": 1}, {"a": {"$type": "string"}})
    assert matches({"a": 1.5}, {"a": {"$type": "double"}})
    assert matches({"a": ObjectId()}, {"a": {"$type": "objectId"}})


def test_type_number_alias() -> None:
    assert matches({"a": 1}, {"a": {"$type": "number"}})
    assert matches({"a": 1.5}, {"a": {"$type": "number"}})
    assert matches({"a": Decimal128("1.5")}, {"a": {"$type": "number"}})
    assert not matches({"a": "x"}, {"a": {"$type": "number"}})


def test_type_list_of_aliases() -> None:
    assert matches({"a": "hi"}, {"a": {"$type": ["string", "int"]}})
    assert matches({"a": 1}, {"a": {"$type": ["string", "int"]}})
    assert not matches({"a": 1.5}, {"a": {"$type": ["string", "int"]}})


def test_size() -> None:
    assert matches({"tags": [1, 2, 3]}, {"tags": {"$size": 3}})
    assert not matches({"tags": [1, 2]}, {"tags": {"$size": 3}})
    assert not matches({"tags": "abc"}, {"tags": {"$size": 3}})


def test_all() -> None:
    assert matches({"tags": ["a", "b", "c"]}, {"tags": {"$all": ["a", "b"]}})
    assert not matches({"tags": ["a"]}, {"tags": {"$all": ["a", "b"]}})


def test_mod() -> None:
    assert matches({"n": 12}, {"n": {"$mod": [4, 0]}})
    assert matches({"n": 13}, {"n": {"$mod": [4, 1]}})
    assert not matches({"n": 13}, {"n": {"$mod": [4, 0]}})


def test_mod_on_array_element() -> None:
    assert matches({"vals": [3, 7, 12]}, {"vals": {"$mod": [4, 0]}})


def test_size_requires_int() -> None:
    with pytest.raises(QueryError):
        matches({"a": [1, 2]}, {"a": {"$size": "two"}})


def test_combine_regex_and_other_operators() -> None:
    doc = {"name": "alice", "age": 30}
    assert matches(doc, {"name": {"$regex": "^ali"}, "age": {"$gte": 18}})


def test_expr_compares_two_fields() -> None:
    assert matches({"a": 5, "b": 3}, {"$expr": {"$gt": ["$a", "$b"]}})
    assert not matches({"a": 1, "b": 3}, {"$expr": {"$gt": ["$a", "$b"]}})


def test_expr_with_arithmetic() -> None:
    doc = {"price": 100, "discount": 30}
    expr = {"$expr": {"$lt": [{"$subtract": ["$price", "$discount"]}, 80]}}
    assert matches(doc, expr)
    assert not matches({"price": 200, "discount": 30}, expr)


def test_expr_returns_falsy_for_missing_field() -> None:
    assert not matches({}, {"$expr": "$missing"})
    assert not matches({"x": None}, {"$expr": "$x"})


def test_expr_combined_with_other_clauses() -> None:
    doc = {"a": 5, "b": 3, "name": "alice"}
    assert matches(doc, {"name": "alice", "$expr": {"$gt": ["$a", "$b"]}})


def test_elem_match_subdoc_form() -> None:
    doc = {"items": [{"sku": "a", "qty": 1}, {"sku": "b", "qty": 5}]}
    assert matches(doc, {"items": {"$elemMatch": {"sku": "b", "qty": {"$gte": 5}}}})
    assert not matches(doc, {"items": {"$elemMatch": {"sku": "b", "qty": {"$gt": 5}}}})


def test_elem_match_scalar_form() -> None:
    doc = {"vals": [1, 5, 10]}
    assert matches(doc, {"vals": {"$elemMatch": {"$gte": 3, "$lt": 7}}})
    assert not matches(doc, {"vals": {"$elemMatch": {"$gte": 11}}})


def test_elem_match_requires_single_element_match() -> None:
    doc = {"items": [{"a": 5, "b": 10}, {"a": -1, "b": 2}]}
    assert not matches(doc, {"items": {"$elemMatch": {"a": {"$gt": 0}, "b": {"$lt": 5}}}})
    doc2 = {"items": [{"a": 5, "b": 2}]}
    assert matches(doc2, {"items": {"$elemMatch": {"a": {"$gt": 0}, "b": {"$lt": 5}}}})


def test_comment_is_ignored() -> None:
    doc = {"a": 1}
    assert matches(doc, {"a": 1, "$comment": "for analytics"})
    assert not matches(doc, {"a": 2, "$comment": "for analytics"})


def test_bits_all_set_with_int_mask() -> None:
    assert matches({"flags": 0b1011}, {"flags": {"$bitsAllSet": 0b1010}})
    assert not matches({"flags": 0b1001}, {"flags": {"$bitsAllSet": 0b1010}})


def test_bits_all_set_with_position_list() -> None:
    assert matches({"flags": 0b1011}, {"flags": {"$bitsAllSet": [0, 1, 3]}})
    assert not matches({"flags": 0b1011}, {"flags": {"$bitsAllSet": [2]}})


def test_bits_any_set() -> None:
    assert matches({"flags": 0b0010}, {"flags": {"$bitsAnySet": 0b1010}})
    assert not matches({"flags": 0b0001}, {"flags": {"$bitsAnySet": 0b1010}})


def test_bits_all_clear() -> None:
    assert matches({"flags": 0b0001}, {"flags": {"$bitsAllClear": 0b1010}})
    assert not matches({"flags": 0b0011}, {"flags": {"$bitsAllClear": 0b1010}})


def test_bits_any_clear() -> None:
    assert matches({"flags": 0b1010}, {"flags": {"$bitsAnyClear": 0b1011}})
    assert not matches({"flags": 0b1111}, {"flags": {"$bitsAnyClear": 0b1010}})


def test_bits_skip_non_int_values() -> None:
    assert not matches({"flags": "abc"}, {"flags": {"$bitsAllSet": 0b1}})
    assert not matches({"flags": True}, {"flags": {"$bitsAllSet": 0b1}})


def test_json_schema_required() -> None:
    schema = {"required": ["name"], "properties": {"name": {"bsonType": "string"}}}
    assert matches({"name": "alice"}, {"$jsonSchema": schema})
    assert not matches({"age": 30}, {"$jsonSchema": schema})


def test_json_schema_property_type_check() -> None:
    schema = {"properties": {"age": {"bsonType": "int", "minimum": 0}}}
    assert matches({"age": 30}, {"$jsonSchema": schema})
    assert not matches({"age": "thirty"}, {"$jsonSchema": schema})
    assert not matches({"age": -1}, {"$jsonSchema": schema})


def test_json_schema_string_length_and_pattern() -> None:
    schema = {
        "properties": {
            "code": {"bsonType": "string", "minLength": 2, "maxLength": 4, "pattern": "^[A-Z]+$"}
        }
    }
    assert matches({"code": "ABC"}, {"$jsonSchema": schema})
    assert not matches({"code": "A"}, {"$jsonSchema": schema})
    assert not matches({"code": "abc"}, {"$jsonSchema": schema})


def test_json_schema_enum() -> None:
    schema = {"properties": {"status": {"enum": ["active", "inactive"]}}}
    assert matches({"status": "active"}, {"$jsonSchema": schema})
    assert not matches({"status": "pending"}, {"$jsonSchema": schema})


def test_json_schema_array_items() -> None:
    schema = {"properties": {"tags": {"bsonType": "array", "items": {"bsonType": "string"}}}}
    assert matches({"tags": ["a", "b"]}, {"$jsonSchema": schema})
    assert not matches({"tags": ["a", 1]}, {"$jsonSchema": schema})


def test_eq_decimal128_matches_int() -> None:
    assert matches({"x": Decimal128("5")}, {"x": 5})
    assert matches({"x": 5}, {"x": Decimal128("5")})


def test_eq_decimal128_matches_float() -> None:
    assert matches({"x": Decimal128("3.5")}, {"x": 3.5})
    assert matches({"x": 3.5}, {"x": Decimal128("3.5")})


def test_eq_decimal128_distinct_values() -> None:
    assert not matches({"x": Decimal128("5")}, {"x": 6})
    assert not matches({"x": Decimal128("3.5")}, {"x": 3})


def test_eq_decimal128_in_array() -> None:
    assert matches({"x": [Decimal128("5"), Decimal128("6")]}, {"x": 6})


def test_eq_decimal128_in_in_clause() -> None:
    assert matches({"x": Decimal128("5")}, {"x": {"$in": [3, 5, 7]}})
    assert not matches({"x": Decimal128("5")}, {"x": {"$in": [3, 7]}})


def test_eq_bool_does_not_bridge_to_int() -> None:
    """MongoDB ranks bool separately from numbers — they should not equate."""
    assert not matches({"x": True}, {"x": 1})
    assert not matches({"x": 1}, {"x": True})
    assert not matches({"x": False}, {"x": 0})


def test_gt_decimal128_vs_int() -> None:
    assert matches({"x": Decimal128("3.5")}, {"x": {"$gt": 2}})
    assert matches({"x": 3.5}, {"x": {"$gt": Decimal128("2")}})
    assert not matches({"x": Decimal128("1.5")}, {"x": {"$gt": 2}})


def test_lte_decimal128_vs_float() -> None:
    assert matches({"x": Decimal128("3.5")}, {"x": {"$lte": 4.0}})
    assert not matches({"x": Decimal128("4.1")}, {"x": {"$lte": 4.0}})
