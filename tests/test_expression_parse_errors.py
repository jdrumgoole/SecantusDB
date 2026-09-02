"""Expression ARITY and SPEC SHAPE are parse errors, not fold errors.

mongod builds the expression tree before it folds anything, so an argument-count
or spec-shape mistake is reported under the STAGE's wrapper (`Invalid $addFields
:: caused by ::`) rather than the optimizer's (`Failed to optimize pipeline`).
We folded first, so 279 shapes carried both the wrong wrapper and the wrong code.

Every value here is pinned against mongod 8.2.11 (2026-09-02) via
`tools/probes/agg_expressions.py`, which went from 551 wrong error codes to 50.
"""

import datetime as dt

import pytest
from bson import Decimal128, Int64, ObjectId, Timestamp

from secantus.aggregate import expression_problem_in_pipeline
from secantus.expressions import ExpressionError, evaluate


def stage_error(expr):
    """The `(code, message)` the PARSE pass reports for this expression.

    Deliberately not `apply_pipeline`: these are errors mongod raises while
    building the expression tree, so they are found by the pre-pass the command
    layer runs before any document is touched. Calling the evaluator instead
    would test a different code path from the one the fix is in.
    """
    found = expression_problem_in_pipeline([{"$addFields": {"r": expr}}], frozenset(), None)
    assert found is not None, f"expected a parse error for {expr!r}"
    code, message, _wrapper = found
    return code, message


class TestDatesFromTypesThatCarryATimestamp:
    """mongod accepts every BSON type with a timestamp inside it."""

    OID = ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")  # generated 2023-07-19 14:56:34 UTC

    def test_an_objectid_is_a_date(self):
        """This used to raise 16006 -- an error where mongod returns a value."""
        assert evaluate({"$year": self.OID}, {}) == 2023
        assert evaluate({"$dayOfMonth": self.OID}, {}) == 19
        assert evaluate({"$hour": self.OID}, {}) == 14

    def test_a_timestamp_is_a_date(self):
        assert evaluate({"$year": Timestamp(1689778594, 1)}, {}) == 2023

    def test_a_real_date_still_works(self):
        assert evaluate({"$year": dt.datetime(2023, 7, 19)}, {}) == 2023

    @pytest.mark.parametrize("value", [5, "x", True, 1.5, Decimal128("1.5")])
    def test_everything_else_is_still_16006(self, value):
        with pytest.raises(ExpressionError) as exc:
            evaluate({"$year": value}, {})
        assert exc.value.code == 16006


class TestArity:
    @pytest.mark.parametrize(
        ("expr", "message"),
        [
            (
                {"$indexOfArray": [[1, 2]]},
                "Expression $indexOfArray takes at least 2 arguments, and at most 4, "
                "but 1 were passed in.",
            ),
            (
                {"$indexOfArray": [[1, 2], 1, 2, 3, 4]},
                "Expression $indexOfArray takes at least 2 arguments, and at most 4, "
                "but 5 were passed in.",
            ),
            (
                {"$range": [1]},
                "Expression $range takes at least 2 arguments, and at most 3, "
                "but 1 were passed in.",
            ),
            (
                {"$slice": [[1, 2]]},
                "Expression $slice takes at least 2 arguments, and at most 3, "
                "but 1 were passed in.",
            ),
        ],
    )
    def test_counts_are_named(self, expr, message):
        code, text = stage_error(expr)
        assert code == 28667
        assert message in text

    def test_a_non_array_argument_counts_as_one(self):
        code, text = stage_error({"$indexOfCP": "x"})
        assert code == 28667
        assert "but 1 were passed in." in text


class TestDateExtractorGivenAnArray:
    """A one-element array is the argument; any other length is 40536."""

    def test_one_element_is_accepted(self):
        assert evaluate({"$year": [dt.datetime(2023, 7, 19)]}, {}) == 2023

    @pytest.mark.parametrize("n", [0, 2, 3])
    def test_any_other_length_is_rejected(self, n):
        code, text = stage_error({"$dayOfMonth": [1] * n})
        assert code == 40536
        assert (
            f"$dayOfMonth accepts exactly one argument if given an array, but was given {n}" in text
        )


class TestObjectSpecExpressions:
    """Each has its OWN Location code -- they do not share one.

    `$sortArray` and `$setField` are excluded deliberately: they take an object
    spec too, but word it differently, and their existing checks were already
    right. Folding them into this family would have broken two correct
    messages -- which the first draft of the fix did.
    """

    @pytest.mark.parametrize(
        ("op", "code"),
        [
            ("$firstN", 5787801),
            ("$lastN", 5787801),
            ("$minN", 5787900),
            ("$maxN", 5787900),
            ("$median", 7436201),
            ("$percentile", 7436200),
            ("$topN", 168),
            ("$bottomN", 168),
        ],
    )
    def test_a_non_object_spec_names_its_own_code(self, op, code):
        got_code, text = stage_error({op: 0})
        assert got_code == code
        assert f"specification must be an object; found {op}: 0" in text


class TestUnrecognisedDateArguments:
    @pytest.mark.parametrize(
        ("op", "code"),
        [
            ("$dateAdd", 5166401),
            ("$dateSubtract", 5166401),
            ("$dateDiff", 5166302),
            ("$dateFromParts", 40518),
            ("$dateToParts", 40520),
            ("$dateFromString", 40541),
            ("$dateToString", 18534),
            ("$dateTrunc", 5439008),
        ],
    )
    def test_each_carries_its_own_code(self, op, code):
        got_code, text = stage_error({op: {"k": 1}})
        assert got_code == code
        assert f"Unrecognized argument to {op}: k" in text

    def test_two_of_them_append_what_they_expected(self):
        _, text = stage_error({"$dateAdd": {"k": 1}})
        assert "Expected arguments are startDate, unit, amount, and optionally timezone." in text


class TestFamiliesThatLookUniformAndAreNot:
    """Each of these has one member that behaves differently from its siblings."""

    @pytest.mark.parametrize("op", ["$bitAnd", "$bitOr", "$bitXor"])
    def test_the_bit_siblings_name_no_type_at_all(self, op):
        code, text = stage_error({op: [1, "abc"]})
        assert code == 14
        assert text.endswith(f"{op} only supports int and long operands.")

    def test_but_bitNot_splits_by_whether_it_is_numeric(self):
        assert stage_error({"$bitNot": "abc"})[0] == 28765
        code, text = stage_error({"$bitNot": 1.5})
        assert code == 14
        assert "only supports int and long, not: double." in text

    def test_setEquals_checks_arity_before_types(self):
        code, text = stage_error({"$setEquals": [[1]]})
        assert code == 17045
        assert "$setEquals needs at least two arguments had: 1" in text

    def test_and_numbers_the_offending_argument(self):
        code, text = stage_error({"$setEquals": [[1], 2]})
        assert code == 5887502
        assert "2-th argument is of type: int" in text

    @pytest.mark.parametrize(("op", "code"), [("$setUnion", 17043), ("$setIntersection", 17047)])
    def test_but_its_siblings_accept_one_array_and_do_not_number(self, op, code):
        assert evaluate({op: [[1]]}, {}) == [1]
        got_code, text = stage_error({op: [[1], 2]})
        assert got_code == code
        assert "One argument is of type: int" in text


class TestRand:
    """`$rand` is checked by the EVALUATOR, not the parse pass -- it is
    non-deterministic, so mongod never folds it."""

    def test_an_empty_document_or_array_is_the_no_argument_form(self):
        assert 0.0 <= evaluate({"$rand": {}}, {}) < 1.0
        assert 0.0 <= evaluate({"$rand": []}, {}) < 1.0

    def test_arguments_are_refused(self):
        with pytest.raises(ExpressionError) as exc:
            evaluate({"$rand": [1]}, {})
        assert exc.value.code == 3040501
        assert "$rand does not currently accept arguments" in str(exc.value)

    @pytest.mark.parametrize("arg", [0, "x", True, None])
    def test_a_non_container_is_a_different_error(self, arg):
        with pytest.raises(ExpressionError) as exc:
            evaluate({"$rand": arg}, {})
        assert exc.value.code == 10065
        assert "invalid parameter: expected an object ($rand)" in str(exc.value)


class TestGetField:
    def test_a_bare_string_still_reads_the_field(self):
        assert evaluate({"$getField": "s"}, {"s": "x"}) == "x"

    @pytest.mark.parametrize(("arg", "type_name"), [(0, "int"), ([1], "array"), (None, "null")])
    def test_a_bare_non_string_reports_the_FIELD_type(self, arg, type_name):
        code, text = stage_error({"$getField": arg})
        assert code == 3041704
        assert f"requires 'field' to evaluate to type String, but got {type_name}" in text

    def test_an_unknown_argument_outranks_everything(self):
        code, text = stage_error({"$getField": {"k": 1}})
        assert code == 3041701
        assert "$getField found an unknown argument: k" in text

    def test_input_is_required_in_the_object_form(self):
        code, text = stage_error({"$getField": {"field": 1}})
        assert code == 3041703
        assert "$getField requires 'input' to be specified" in text


class TestIndexOfCP:
    @pytest.mark.parametrize(
        ("args", "code", "which"),
        [([3, 1, 2], 40093, "first"), (["ab", 1], 40094, "second")],
    )
    def test_the_offending_argument_is_named(self, args, code, which):
        got_code, text = stage_error({"$indexOfCP": args})
        assert got_code == code
        assert f"$indexOfCP requires a string as the {which} argument, found: int" in text


class TestIntegersStillWork:
    """A guard against over-eager validation breaking working expressions."""

    def test_a_representative_set_of_valid_expressions(self):
        assert evaluate({"$indexOfCP": ["abc", "b"]}, {}) == 1
        assert evaluate({"$range": [0, 3]}, {}) == [0, 1, 2]
        assert evaluate({"$slice": [[1, 2, 3], 2]}, {}) == [1, 2]
        assert evaluate({"$setEquals": [[1], [1]]}, {}) is True
        assert evaluate({"$bitAnd": [Int64(6), Int64(3)]}, {}) == 2
        assert evaluate({"$bitNot": Int64(0)}, {}) == -1
