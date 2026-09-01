"""`$addToSet` membership is BSON value equality, not Python `==`.

Pinned against mongod 8.2.11 (2026-09-01). The Rust engine used to DEFER every
one of these -- a bool or a document anywhere in the array or the argument --
on the strength of a comment saying `py_eq` "mirrors Python's `==`". It had
stopped doing so; only document field ORDER was still missing. A defer is not
free on the standalone Rust server, where nothing sits behind the fallback, so
`$addToSet: true` answered `BadValue` instead of appending.
"""

import pytest
from bson import Code, Decimal128, Int64, ObjectId, Regex

from secantus.update import apply_update


def add(existing, item):
    return apply_update({"a": list(existing)}, {"$addToSet": {"a": item}})["a"]


class TestNumericTypesUnify:
    @pytest.mark.parametrize("item", [1, 1.0, Int64(1), Decimal128("1")])
    def test_one_dedups_against_one_whatever_its_width(self, item):
        assert add([1], item) == [1]


class TestBoolIsItsOwnType:
    def test_true_is_not_one(self):
        assert add([1], True) == [1, True]

    def test_one_is_not_true(self):
        assert add([True], 1) == [True, 1]

    def test_false_is_not_zero(self):
        assert add([0], False) == [0, False]


class TestDocumentsCompareInFieldOrder:
    def test_same_order_dedups(self):
        assert add([{"x": 1, "y": 2}], {"x": 1, "y": 2}) == [{"x": 1, "y": 2}]

    def test_reordered_is_a_different_value(self):
        assert add([{"x": 1, "y": 2}], {"y": 2, "x": 1}) == [
            {"x": 1, "y": 2},
            {"y": 2, "x": 1},
        ]

    def test_the_rule_recurses(self):
        got = add([{"d": {"x": 1, "y": 2}}], {"d": {"y": 2, "x": 1}})
        assert got == [{"d": {"x": 1, "y": 2}}, {"d": {"y": 2, "x": 1}}]

    def test_a_longer_document_is_different(self):
        assert add([{"x": 1}], {"x": 1, "y": 2}) == [{"x": 1}, {"x": 1, "y": 2}]

    def test_empty_documents_dedup(self):
        assert add([{}], {}) == [{}]


class TestArraysAreOrderSensitive:
    def test_same_order_dedups(self):
        assert add([[1, 2]], [1, 2]) == [[1, 2]]

    def test_reordered_appends(self):
        assert add([[1, 2]], [2, 1]) == [[1, 2], [2, 1]]


class TestExoticValues:
    def test_code_is_not_the_string_it_wraps(self):
        assert add(["ab"], Code("ab")) == ["ab", Code("ab")]

    def test_and_the_string_is_not_the_code(self):
        assert add([Code("ab")], "ab") == [Code("ab"), "ab"]

    def test_code_dedups_against_equal_code(self):
        assert add([Code("ab")], Code("ab")) == [Code("ab")]

    def test_regexes_dedup_on_pattern_and_option_set(self):
        assert add([Regex("ab", "i")], Regex("ab", "i")) == [Regex("ab", "i")]

    def test_different_options_append(self):
        got = add([Regex("ab", "i")], Regex("ab", "m"))
        assert [(r.pattern, r.flags) for r in got] == [("ab", 2), ("ab", 8)]

    def test_null_dedups(self):
        assert add([None], None) == [None]

    def test_objectids_dedup_by_value(self):
        oid = ObjectId("0" * 24)
        assert add([oid], ObjectId("0" * 24)) == [oid]
