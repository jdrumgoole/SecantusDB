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


def test_in_nin_argument_validation() -> None:
    # mongod: $in/$nin need an array (else BadValue), and no element may be a
    # document with a $-prefixed key ("cannot nest $ under $in").
    for op in ("$in", "$nin"):
        with pytest.raises(QueryError) as exc:
            matches({"a": 5}, {"a": {op: 5}})
        assert exc.value.code == 2 and "needs an array" in str(exc.value)
    for bad in ({"$regex": "x"}, {"$x": 1}):
        with pytest.raises(QueryError) as exc:
            matches({"a": 5}, {"a": {"$in": [1, bad]}})
        assert exc.value.code == 2 and "cannot nest $ under $in" in str(exc.value)
    # A plain subdocument element and a regex literal are still valid.
    assert not matches({"a": 5}, {"a": {"$in": [{"x": 1}]}})
    assert matches({"a": "hi"}, {"a": {"$in": [Regex("^h")]}})


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


def test_and_or_nor_malformed_raises() -> None:
    """``$and`` / ``$or`` / ``$nor`` require a non-empty array of sub-documents.
    A non-list, an empty list, or a non-document element is a parse error
    (``QueryError`` → BadValue 2 on the wire), not a Python ``TypeError`` that
    leaks out as a generic InternalError."""
    for op in ("$and", "$or", "$nor"):
        with pytest.raises(QueryError):
            matches({"a": 1}, {op: True})
        with pytest.raises(QueryError):
            matches({"a": 1}, {op: 5})
        with pytest.raises(QueryError):
            matches({"a": 1}, {op: []})
        with pytest.raises(QueryError):
            matches({"a": 1}, {op: [True]})


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


def test_type_argument_validation() -> None:
    # mongod: unknown alias / out-of-range / fractional code -> 2, bool -> 14.
    for t in ("notatype", 0, 100, 2.5):
        with pytest.raises(QueryError) as exc:
            matches({"a": 5}, {"a": {"$type": t}})
        assert exc.value.code == 2, t
    with pytest.raises(QueryError) as exc:
        matches({"a": 5}, {"a": {"$type": True}})
    assert exc.value.code == 14
    # code 0 carries the $exists hint; an array validates each element.
    with pytest.raises(QueryError) as exc:
        matches({"a": 5}, {"a": {"$type": 0}})
    assert "Instead use {$exists:false}" in str(exc.value)
    with pytest.raises(QueryError):
        matches({"a": 5}, {"a": {"$type": ["int", "notatype"]}})
    # Valid numeric codes (incl. a whole double and minKey -1) are accepted.
    assert matches({"a": 5}, {"a": {"$type": 16}})
    assert matches({"a": 5.0}, {"a": {"$type": 1.0}})  # double code -> matches double
    assert not matches({"a": 5}, {"a": {"$type": -1}})  # minKey: valid, no match


def test_size() -> None:
    assert matches({"tags": [1, 2, 3]}, {"tags": {"$size": 3}})
    assert not matches({"tags": [1, 2]}, {"tags": {"$size": 3}})
    assert not matches({"tags": "abc"}, {"tags": {"$size": 3}})


def test_size_argument_validation() -> None:
    """$size validates its argument like mongod 7.0.12: an integer-valued float
    is accepted (2.0 == 2); a non-integer float, a negative value, a string, or
    a bool each raise a parse error (previously a negative silently no-matched,
    a bool was accepted as 1, and 2.0 was wrongly rejected)."""
    # Integer-valued float accepted.
    assert matches({"a": [1, 2]}, {"a": {"$size": 2.0}})
    assert not matches({"a": [1]}, {"a": {"$size": 2.0}})
    # Each invalid argument raises.
    for bad in (-1, 2.5, "2", True):
        with pytest.raises(QueryError):
            matches({"a": [1, 2]}, {"a": {"$size": bad}})


def test_all() -> None:
    assert matches({"tags": ["a", "b", "c"]}, {"tags": {"$all": ["a", "b"]}})
    assert not matches({"tags": ["a"]}, {"tags": {"$all": ["a", "b"]}})


def test_all_scalar_field_and_empty() -> None:
    """mongod treats a *scalar* field like a one-element array for ``$all``
    (verified against mongod 7.0.12): ``{tags: {$all: ["red"]}}`` matches both
    ``tags: ["red", ...]`` and scalar ``tags: "red"`` — previously the scalar
    was silently missed. Regex elements match the scalar as a pattern too. An
    empty ``$all`` matches nothing (not everything), and ``$elemMatch`` clauses
    still require an actual array."""
    from bson import Regex

    assert matches({"tags": "red"}, {"tags": {"$all": ["red"]}})
    assert matches({"tags": "red"}, {"tags": {"$all": [Regex("^red$")]}})
    assert not matches({"tags": "red"}, {"tags": {"$all": ["red", "blue"]}})
    # $all: [] matches nothing.
    assert not matches({"tags": ["a", "b"]}, {"tags": {"$all": []}})
    assert not matches({"tags": "red"}, {"tags": {"$all": []}})
    # $elemMatch requires an array — a scalar never satisfies it.
    assert not matches({"tags": "red"}, {"tags": {"$all": [{"$elemMatch": {"$eq": "red"}}]}})
    assert matches({"tags": ["red"]}, {"tags": {"$all": [{"$elemMatch": {"$eq": "red"}}]}})


def test_all_with_elemmatch() -> None:
    q = {"a": {"$all": [{"$elemMatch": {"$gt": 1, "$lt": 3}}]}}
    assert matches({"a": [1, 2, 3]}, q)  # 2 is in (1, 3)
    assert not matches({"a": [4, 5]}, q)
    # Two clauses: array must have an element for each (may differ).
    q2 = {"a": {"$all": [{"$elemMatch": {"$gt": 4}}, {"$elemMatch": {"$lt": 2}}]}}
    assert matches({"a": [1, 5, 10]}, q2)
    assert not matches({"a": [5, 10]}, q2)  # nothing < 2


def test_all_argument_validation() -> None:
    # mongod: $all needs an array; if any element is a $-expression it must be an
    # all-$elemMatch form — a mixed or other $-op element is "no $ expressions".
    with pytest.raises(QueryError) as exc:
        matches({"a": [1, 2]}, {"a": {"$all": 5}})
    assert exc.value.code == 2 and "needs an array" in str(exc.value)
    for bad in (
        {"a": {"$all": [1, {"$elemMatch": {"x": 1}}]}},  # mixed
        {"a": {"$all": [{"$gt": 1}]}},  # non-elemMatch $-doc
    ):
        with pytest.raises(QueryError) as exc:
            matches({"a": [1, 2]}, bad)
        assert exc.value.code == 2 and "no $ expressions in $all" in str(exc.value)
    # A pure-scalar form and an all-$elemMatch form remain valid.
    assert matches({"a": [1, 2, 3]}, {"a": {"$all": [1, 2]}})
    assert matches({"a": [1, 2, 3]}, {"a": {"$all": [{"$elemMatch": {"$gt": 2}}]}})


def test_in_with_regex() -> None:
    # A regex candidate matches string values by pattern (not by equality).
    assert matches({"s": "hello"}, {"s": {"$in": [Regex("^h", "i")]}})
    assert matches({"s": "HELLO"}, {"s": {"$in": [Regex("^h", "i")]}})
    assert not matches({"s": "world"}, {"s": {"$in": [Regex("^h", "i")]}})
    # Mixed literal + regex candidates.
    assert matches({"s": "abc"}, {"s": {"$in": ["x", Regex("^a")]}})
    # $nin is the negation.
    assert matches({"s": "world"}, {"s": {"$nin": [Regex("^h")]}})
    assert not matches({"s": "hello"}, {"s": {"$nin": [Regex("^h")]}})


def test_not_argument_validation() -> None:
    # mongod: $not needs a regex or a non-empty document; scalar/array/bool ->
    # "needs a regex or a document", empty doc -> "cannot be empty" (both code 2).
    for bad in (5, "x", [], True):
        with pytest.raises(QueryError) as exc:
            matches({"a": 5}, {"a": {"$not": bad}})
        assert exc.value.code == 2 and "needs a regex or a document" in str(exc.value)
    with pytest.raises(QueryError) as exc:
        matches({"a": 5}, {"a": {"$not": {}}})
    assert exc.value.code == 2 and "cannot be empty" in str(exc.value)
    # A regex and an operator document are valid.
    assert matches({"a": 5}, {"a": {"$not": Regex("x")}})
    assert not matches({"a": 5}, {"a": {"$not": {"$gt": 3}}})


def test_elemmatch_argument_validation() -> None:
    # mongod: $elemMatch needs an Object; a scalar/array is BadValue.
    for bad in (5, "x", [1, 2]):
        with pytest.raises(QueryError) as exc:
            matches({"a": [1, 2, 3]}, {"a": {"$elemMatch": bad}})
        assert exc.value.code == 2 and "needs an Object" in str(exc.value)
    assert matches({"a": [1, 2, 3]}, {"a": {"$elemMatch": {"$gt": 2}}})


def test_regex_options_validation() -> None:
    # mongod: bad flag -> 51108; non-string $options -> 2; $options without
    # $regex -> 2; non-string $regex -> 2. Valid flags imsxu still work.
    with pytest.raises(QueryError) as exc:
        matches({"s": "hi"}, {"s": {"$regex": "h", "$options": "z"}})
    assert exc.value.code == 51108 and "invalid flag" in str(exc.value)
    for q in (
        {"s": {"$regex": "h", "$options": 5}},
        {"s": {"$options": "i"}},
        {"s": {"$regex": 5}},
        {"s": {"$regex": None}},
    ):
        with pytest.raises(QueryError) as exc:
            matches({"s": "hi"}, q)
        assert exc.value.code == 2, q
    # Valid options and an empty option string are accepted.
    assert matches({"s": "Hello"}, {"s": {"$regex": "^h", "$options": "i"}})
    assert matches({"s": "hello"}, {"s": {"$regex": "^h", "$options": ""}})


def test_mod() -> None:
    assert matches({"n": 12}, {"n": {"$mod": [4, 0]}})
    assert matches({"n": 13}, {"n": {"$mod": [4, 1]}})
    assert not matches({"n": 13}, {"n": {"$mod": [4, 0]}})


def test_mod_truncation_bool_and_errors() -> None:
    """$mod fidelity, pinned against mongod 7.0.12: value AND divisor truncate
    toward zero to integers; bool is excluded (not a number); C-style
    (truncated) modulo, so -5 % 2 == -1; Decimal128 counts on the Python
    engine; divisor 0 and a malformed spec raise."""
    from bson import Decimal128

    # Double values truncate toward zero (previously the Rust server errored).
    assert matches({"n": 5.0}, {"n": {"$mod": [2, 1]}})
    assert matches({"n": 5.5}, {"n": {"$mod": [2, 1]}})  # trunc 5
    assert matches({"n": 4.9}, {"n": {"$mod": [2, 0]}})  # trunc 4
    # The divisor truncates too: [2.5, 0] divides by 2.
    assert matches({"n": 4.9}, {"n": {"$mod": [2.5, 0]}})
    # bool is excluded (Python's True % 2 would have matched).
    assert not matches({"n": True}, {"n": {"$mod": [2, 1]}})
    # C-style modulo: -5 % 2 == -1, not 1.
    assert not matches({"n": -5}, {"n": {"$mod": [2, 1]}})
    assert matches({"n": -5}, {"n": {"$mod": [2, -1]}})
    # Decimal128 counts (Python engine).
    assert matches({"n": Decimal128("5")}, {"n": {"$mod": [2, 1]}})
    # Errors.
    with pytest.raises(QueryError):
        matches({"n": 5}, {"n": {"$mod": [0, 1]}})
    with pytest.raises(QueryError):
        matches({"n": 5}, {"n": {"$mod": [2]}})


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


def test_json_schema_unique_items() -> None:
    schema = {"properties": {"tags": {"bsonType": "array", "uniqueItems": True}}}
    assert matches({"tags": ["a", "b", "c"]}, {"$jsonSchema": schema})
    assert matches({"tags": []}, {"$jsonSchema": schema})
    assert not matches({"tags": ["a", "b", "a"]}, {"$jsonSchema": schema})
    # cross-type-equal numerics collide (1 == 1.0) at the top level
    assert not matches({"tags": [1, 1.0]}, {"$jsonSchema": schema})
    # distinct vs duplicate documents
    assert matches({"tags": [{"x": 1}, {"x": 2}]}, {"$jsonSchema": schema})
    assert not matches({"tags": [{"x": 1}, {"x": 1}]}, {"$jsonSchema": schema})
    # nested cross-type-equal numerics collide ({a: 1} == {a: 1.0})
    assert not matches({"tags": [{"a": 1}, {"a": 1.0}]}, {"$jsonSchema": schema})
    assert matches({"tags": [{"a": 1}, {"a": 2}]}, {"$jsonSchema": schema})
    # and recursively inside sub-arrays
    assert not matches({"tags": [[1, 2], [1.0, 2.0]]}, {"$jsonSchema": schema})
    assert matches({"tags": [[1, 2], [1, 3]]}, {"$jsonSchema": schema})
    # uniqueItems: false is a no-op
    off = {"properties": {"tags": {"uniqueItems": False}}}
    assert matches({"tags": [1, 1]}, {"$jsonSchema": off})


def test_json_schema_all_of() -> None:
    schema = {"properties": {"n": {"allOf": [{"bsonType": "int"}, {"minimum": 0}]}}}
    assert matches({"n": 5}, {"$jsonSchema": schema})
    assert not matches({"n": -1}, {"$jsonSchema": schema})  # fails minimum
    assert not matches({"n": 1.5}, {"$jsonSchema": schema})  # fails bsonType


def test_json_schema_any_of() -> None:
    schema = {"properties": {"x": {"anyOf": [{"bsonType": "string"}, {"bsonType": "int"}]}}}
    assert matches({"x": "s"}, {"$jsonSchema": schema})
    assert matches({"x": 3}, {"$jsonSchema": schema})
    assert not matches({"x": 1.5}, {"$jsonSchema": schema})


def test_json_schema_one_of() -> None:
    schema = {"properties": {"n": {"oneOf": [{"bsonType": "int"}, {"bsonType": "string"}]}}}
    assert matches({"n": 5}, {"$jsonSchema": schema})  # exactly one (int)
    assert matches({"n": "s"}, {"$jsonSchema": schema})  # exactly one (string)
    assert not matches({"n": 1.5}, {"$jsonSchema": schema})  # neither -> zero matches
    # A value satisfying BOTH sub-schemas -> more than one -> fails. (5 is >= 0
    # and <= 10; a bound-only schema doesn't constrain the other direction.)
    two = {"properties": {"n": {"oneOf": [{"minimum": 0}, {"maximum": 10}]}}}
    assert not matches({"n": 5}, {"$jsonSchema": two})


def test_json_schema_not() -> None:
    schema = {"properties": {"x": {"not": {"bsonType": "int"}}}}
    assert matches({"x": "s"}, {"$jsonSchema": schema})
    assert not matches({"x": 5}, {"$jsonSchema": schema})


def test_json_schema_additional_properties() -> None:
    false_schema = {"properties": {"a": {}}, "additionalProperties": False}
    assert matches({"a": 1}, {"$jsonSchema": false_schema})
    assert not matches({"a": 1, "b": 2}, {"$jsonSchema": false_schema})  # extra `b`
    # a sub-schema validates each additional property
    typed = {"properties": {"a": {}}, "additionalProperties": {"bsonType": "string"}}
    assert matches({"a": 1, "b": "x"}, {"$jsonSchema": typed})
    assert not matches({"a": 1, "b": 2}, {"$jsonSchema": typed})


def test_json_schema_pattern_properties() -> None:
    schema = {"patternProperties": {"^s_": {"bsonType": "string"}}}
    assert matches({"s_name": "x", "n": 5}, {"$jsonSchema": schema})  # s_name str, n ignored
    assert not matches({"s_name": 5}, {"$jsonSchema": schema})  # s_name not a string
    # a pattern-matched key is not "additional"
    strict = {
        "properties": {"id": {}},
        "patternProperties": {"^s_": {}},
        "additionalProperties": False,
    }
    assert matches({"id": 1, "s_x": 2}, {"$jsonSchema": strict})
    assert not matches({"id": 1, "other": 2}, {"$jsonSchema": strict})  # `other` is additional


def test_json_schema_dependencies() -> None:
    # property (list) form: if `card` present, `billing` must be too.
    lst = {"dependencies": {"card": ["billing"]}}
    assert matches({"card": 1, "billing": 2}, {"$jsonSchema": lst})
    assert not matches({"card": 1}, {"$jsonSchema": lst})
    assert matches({"x": 1}, {"$jsonSchema": lst})  # trigger absent -> ok
    # schema form: if `a` present, the doc must validate against the sub-schema.
    sch = {"dependencies": {"a": {"required": ["b"], "properties": {"b": {"bsonType": "int"}}}}}
    assert matches({"a": 1, "b": 2}, {"$jsonSchema": sch})
    assert not matches({"a": 1, "b": "x"}, {"$jsonSchema": sch})


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


def test_range_orders_embedded_documents() -> None:
    """Two embedded documents order field-by-field under range operators
    (mongod 7.0.12-probed): first differing key compares as a string, else
    recurse into the value, else the shorter document sorts first. Previously
    both servers returned no-match (Python's ``operator.gt`` raises on dicts).
    Cross-bracket (document vs scalar) still no-matches."""
    assert matches({"a": {"x": 2}}, {"a": {"$gt": {"x": 1}}})
    assert matches({"a": {"x": 1, "y": 9}}, {"a": {"$gt": {"x": 1}}})  # longer wins on tie
    assert matches({"a": {"y": 1}}, {"a": {"$gt": {"x": 1}}})  # key "y" > "x"
    assert not matches({"a": {"x": 1}}, {"a": {"$gt": {"x": 1}}})  # equal
    assert matches({"a": {"x": 1}}, {"a": {"$gte": {"x": 1}}})
    assert matches({"a": {"x": 0}}, {"a": {"$lt": {"x": 1}}})
    assert matches({"a": {"x": 1}}, {"a": {"$lt": {"x": 1, "y": 5}}})  # shorter sorts first
    # Type bracket: a document field vs a scalar bound, and a scalar field vs a
    # document bound, both no-match.
    assert not matches({"a": {"x": 1}}, {"a": {"$gt": 2}})
    assert not matches({"a": 2}, {"a": {"$gt": {"x": 1}}})


def test_range_orders_arrays_by_full_bson_order() -> None:
    """Two arrays order element-by-element under range, but each element pair
    compares by FULL BSON order (type rank first) — mongod ranks a string
    element above a number, so ``[1, "x"] > [1, 2]``. Previously both servers
    no-matched a cross-type element pair (Python's list ``<`` raises str vs
    int). Verified against mongod 7.0.12."""
    assert matches({"a": [1, "x"]}, {"a": {"$gt": [1, 2]}})  # "x"(str) > 2(num)
    assert not matches({"a": [1, "x"]}, {"a": {"$lt": [1, 2]}})
    assert matches({"a": [2, "x"]}, {"a": {"$gt": [1, 2]}})  # decisive first elem
    assert matches({"a": ["x", 1]}, {"a": {"$gt": [1, 2]}})  # str first > num first
    assert matches({"a": [1]}, {"a": {"$lt": [1, 2]}})  # shorter prefix sorts first


def test_embedded_document_equality_is_ordered_and_exact() -> None:
    """Full embedded-document equality is field-ORDER-sensitive and
    exact (no subset), recursively — a mongod gotcha. Oracle-pinned
    against a real mongod 2026-06-13."""
    doc = {"size": {"h": 14, "w": 21, "uom": "cm"}}
    assert matches(doc, {"size": {"h": 14, "w": 21, "uom": "cm"}})  # same order
    assert not matches(doc, {"size": {"w": 21, "h": 14, "uom": "cm"}})  # reordered
    assert not matches(doc, {"size": {"h": 14, "w": 21}})  # subset
    assert matches(doc, {"size.h": 14})  # dotted still works

    # Numeric bridge applies to leaf values inside the embedded doc.
    assert matches(doc, {"size": {"h": 14.0, "w": 21, "uom": "cm"}})
    # ...but bool stays distinct.
    assert not matches({"s": {"h": 14}}, {"s": {"h": True}})

    # Nested embedded docs are ordered recursively.
    nested = {"a": {"b": {"x": 1, "y": 2}}}
    assert matches(nested, {"a": {"b": {"x": 1, "y": 2}}})
    assert not matches(nested, {"a": {"b": {"y": 2, "x": 1}}})

    # Arrays inside embedded docs are positional (order-sensitive).
    arr = {"s": {"a": [1, 2], "b": 3}}
    assert matches(arr, {"s": {"a": [1, 2], "b": 3}})
    assert not matches(arr, {"s": {"a": [2, 1], "b": 3}})


def test_datetime_naive_aware_equality_same_instant():
    # A BSON date decodes tz-naive UTC; a SQL timestamptz literal arrives tz-aware
    # UTC. Bare equality must match the same instant across the naive/aware boundary
    # (naive is treated as UTC, matching pymongo's BSON encoding). #142
    import datetime as dt

    naive = dt.datetime(2020, 1, 2, 3, 4, 5)
    aware = dt.datetime(2020, 1, 2, 3, 4, 5, tzinfo=dt.timezone.utc)
    assert matches({"at": naive}, {"at": aware})
    assert matches({"at": aware}, {"at": naive})
    assert matches({"at": naive}, {"at": {"$eq": aware}})
    assert matches({"at": naive}, {"at": {"$in": [aware]}})
    assert not matches({"at": naive}, {"at": {"$ne": aware}})
    # A different instant (offset shifts it) must NOT match.
    other = dt.datetime(2020, 1, 2, 3, 4, 5, tzinfo=dt.timezone(dt.timedelta(hours=2)))
    assert not matches({"at": naive}, {"at": other})


def test_json_schema_keyword_validation_errors() -> None:
    """Parse-time $jsonSchema keyword validation, verbatim from a mongod 7.0
    probe: unknown keyword / unsupported keyword / draft-4 exclusive-bound
    rules / multipleOf typing, each with mongod's code."""
    import pytest

    from secantus.query import QueryError

    for schema, code, frag in [
        ({"$ref": "#/x"}, 9, "not currently supported"),
        ({"$schema": "x"}, 9, "not currently supported"),
        ({"format": "email"}, 9, "not currently supported"),
        ({"notakeyword": 1}, 9, "Unknown $jsonSchema keyword: notakeyword"),
        ({"properties": {"n": {"notakeyword": 1}}}, 9, "Unknown $jsonSchema keyword"),
        ({"items": {"notakeyword": 1}}, 9, "Unknown $jsonSchema keyword"),
        ({"allOf": [{"notakeyword": 1}]}, 9, "Unknown $jsonSchema keyword"),
        ({"minimum": 5, "exclusiveMinimum": 6}, 14, "must be a boolean"),
        ({"exclusiveMinimum": True}, 9, "'minimum' must be a present if"),
        ({"exclusiveMaximum": True}, 9, "'maximum' must be a present if"),
        ({"multipleOf": 0}, 9, "must have a positive value"),
        ({"multipleOf": -2}, 9, "must have a positive value"),
        ({"multipleOf": "x"}, 14, "must be a number"),
        ({"multipleOf": True}, 14, "must be a number"),
        ({"title": 5}, 14, "must be of type string"),
        (5, 14, "$jsonSchema must be an object"),
    ]:
        with pytest.raises(QueryError) as exc:
            matches({}, {"$jsonSchema": schema})
        assert exc.value.code == code, schema
        assert frag in str(exc.value), schema


def test_json_schema_draft4_bounds_multipleof_and_tuple_items() -> None:
    """The newly-implemented keywords match mongod's probed semantics."""
    ex_min = {"properties": {"n": {"minimum": 6, "exclusiveMinimum": True}}}
    assert not matches({"n": 6}, {"$jsonSchema": ex_min})
    assert matches({"n": 7}, {"$jsonSchema": ex_min})
    assert matches({"n": 6}, {"$jsonSchema": {"properties": {"n": {"minimum": 6}}}})

    mof = {"properties": {"n": {"multipleOf": 2.5}}}
    assert matches({"n": 7.5}, {"$jsonSchema": mof})
    assert not matches({"n": 6}, {"$jsonSchema": mof})

    tup = {"properties": {"a": {"items": [{"bsonType": "int"}], "additionalItems": False}}}
    assert matches({"a": [1]}, {"$jsonSchema": tup})
    assert not matches({"a": [1, "x"]}, {"$jsonSchema": tup})
    tup_schema = {
        "properties": {
            "a": {"items": [{"bsonType": "int"}], "additionalItems": {"bsonType": "string"}}
        }
    }
    assert matches({"a": [1, "x"]}, {"$jsonSchema": tup_schema})
    assert not matches({"a": [1, True]}, {"$jsonSchema": tup_schema})

    # Metadata keywords are accepted and ignored, top-level and nested.
    meta = {"title": "t", "description": "d", "properties": {"n": {"title": "x", "minimum": 1}}}
    assert matches({"n": 5}, {"$jsonSchema": meta})


def test_bits_numeric_arg_validation() -> None:
    """$bits* accept a whole-number-double mask / bit position (truncated), and
    reject a fractional / negative / bool one — a bad position with code 2, a bad
    non-array mask with code 9. mongod 7.0.12-verified."""
    assert matches({"n": 6}, {"n": {"$bitsAllSet": 6.0}}) is True
    assert matches({"n": 6}, {"n": {"$bitsAllSet": [1.0, 2.0]}}) is True
    for query, code in [
        ({"n": {"$bitsAllSet": 2.5}}, 9),
        ({"n": {"$bitsAllSet": -1}}, 9),
        ({"n": {"$bitsAllSet": True}}, 2),
        ({"n": {"$bitsAllSet": [1.5]}}, 2),
        ({"n": {"$bitsAllSet": [-1]}}, 2),  # was an uncaught ValueError (code None)
        ({"n": {"$bitsAllSet": [True]}}, 2),
        ({"n": {"$bitsAnyClear": 2.5}}, 9),
        ({"n": {"$bitsAllClear": [-1]}}, 2),
    ]:
        with pytest.raises(QueryError) as exc:
            matches({"n": 6}, query)
        assert exc.value.code == code, query


def test_gte_lte_null_and_exists_truthiness() -> None:
    """$gte/$lte: null match null + missing (like $eq: null); $exists uses mongod
    truthiness (only false/0/null are falsy). mongod 7.0.12-verified."""
    docs = [{"_id": 1, "f": None}, {"_id": 2, "f": 5}, {"_id": 3}]

    def ids(q):
        return sorted(d["_id"] for d in docs if matches(d, q))

    assert ids({"f": {"$gte": None}}) == [1, 3]  # null + missing (was [])
    assert ids({"f": {"$lte": None}}) == [1, 3]
    assert ids({"f": {"$gt": None}}) == []  # nothing strictly above null
    assert ids({"f": {"$eq": None}}) == [1, 3]
    # mongod truthiness: empty string / array / doc are TRUTHY (Python's aren't)
    assert ids({"f": {"$exists": ""}}) == [1, 2]
    assert ids({"f": {"$exists": []}}) == [1, 2]
    assert ids({"f": {"$exists": {}}}) == [1, 2]
    assert ids({"f": {"$exists": 0}}) == [3]
    assert ids({"f": {"$exists": False}}) == [3]
    assert ids({"f": {"$exists": 1}}) == [1, 2]


def test_jsonschema_deep_nesting_raises_typed_error_not_recursion() -> None:
    """A pathologically deeply-nested ``$jsonSchema`` recurses through every
    sub-schema. Instead of letting the ``RecursionError`` escape ``matches`` to
    the dispatch layer's blanket handler (a generic InternalError), it is
    translated into a typed ``FailedToParse`` (code 9). (security review
    2026-07-20, I21.)"""
    schema: dict = {"bsonType": "int"}
    for _ in range(5000):
        schema = {"bsonType": "object", "properties": {"a": schema}}
    with pytest.raises(QueryError) as ei:
        matches({"a": 1}, {"$jsonSchema": schema})
    assert ei.value.code == 9
    assert ei.value.code_name == "FailedToParse"
    assert "deep" in str(ei.value)
