"""A wrong-typed operand names mongod's error, on both servers.

An aggregation operator that had to REFUSE an argument could only signal
`Fallback::Defer` in the Rust engine, and a defer on the standalone Rust server
has no Python behind it -- it surfaces as `2 BadValue`, "aggregation pipeline
uses a stage or operator not supported by the Rust server". So `{$size: 1}` told
the client this server cannot do `$size`. It can; 1 is not an array.

`tools/probes/agg_expressions.py` measured 908 such divergences against mongod
8.2.11 on 2026-09-02, across ~120 operators and with **zero wrong values** --
the whole surface was error-shaped. The cases below are the ones that pin a rule
which a reasonable guess gets wrong, every expectation measured against that
server rather than derived:

* the wordings are per-operator and not interchangeable ("found: {}" vs
  "but is {}" vs "but was of type: {}"), and `$tsSecond`'s carries a verbatim
  LEADING SPACE;
* mongod says `missing` for an absent field path where it says `null` for an
  explicit null, which one `eval` collapses together;
* null-tolerance is per operator: `$size` and `$strLenCP` refuse null while
  `$first` and `$reverseArray` answer null for it;
* `$bitNot` uses TWO codes where its `$bitAnd` siblings use one;
* three set operators answer null for a null operand and two refuse it;
* `$getField`'s bare form is an EXPRESSION, not a literal field name, and it
  never folds.

Run against both servers deliberately. Most of these were Rust-only defects, but
the set-operator and `$getField` rules were wrong on the Python server too --
they sit outside the probe corpus, so nothing had ever compared them.
"""

from __future__ import annotations

import contextlib
import datetime

import pytest
from bson import Decimal128, Int64, ObjectId, Timestamp
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer

WHEN = datetime.datetime(2026, 1, 2, 3, 4, 5)
DOC = {"_id": 1, "n": 2, "s": "abc", "arr": [3, 1, 2], "o": {"k": 1}, "t": WHEN}


@contextlib.contextmanager
def _python_client(tmp_path):
    import pymongo

    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        client = pymongo.MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        try:
            yield client
        finally:
            client.close()


@contextlib.contextmanager
def _rust_client(tmp_path):
    import pymongo

    _server = pytest.importorskip("_secantus_server")
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        host, port = srv.address
        client = pymongo.MongoClient(
            host, port, directConnection=True, serverSelectionTimeoutMS=5000
        )
        try:
            yield client
        finally:
            client.close()
    finally:
        srv.stop()


@pytest.fixture(params=["python", "rust"])
def coll(request, tmp_path):
    factory = _python_client if request.param == "python" else _rust_client
    with factory(tmp_path) as client:
        c = client["operr"]["c"]
        c.insert_one(dict(DOC))
        yield c


def _run(coll, expr):
    """`(code, message)` for a refusal, or `("OK", value)` for an answer.

    The pipeline wrappers are stripped: which one applies is its own rule (see
    `test_the_pipeline_wrapper_follows_the_argument`) and mixing it into every
    assertion would hide the sentence being pinned.
    """
    try:
        out = list(coll.aggregate([{"$addFields": {"z": expr}}]))
    except OperationFailure as e:
        return e.code, (e.details or {}).get("errmsg", "").split(":: caused by :: ")[-1]
    return "OK", out[0].get("z", "MISSING")


# Each operator's own sentence, verbatim from mongod 8.2.11. The five phrasings
# here are why this is a table rather than one message with a name substituted.
@pytest.mark.parametrize(
    ("expr", "code", "message"),
    [
        ({"$size": 1}, 17124, "The argument to $size must be an array, but was of type: int"),
        ({"$first": 1}, 28689, "$first's argument must be an array, but is int"),
        ({"$last": 1}, 28689, "$last's argument must be an array, but is int"),
        (
            {"$reverseArray": 1},
            34435,
            "The argument to $reverseArray must be an array, but was of type: int",
        ),
        ({"$strLenCP": 1}, 34471, "$strLenCP requires a string argument, found: int"),
        ({"$strLenBytes": 1}, 34473, "$strLenBytes requires a string argument, found: int"),
        ({"$arrayToObject": 1}, 40386, "$arrayToObject requires an array input, found: int"),
        ({"$objectToArray": 1}, 40390, "$objectToArray requires a document input, found: int"),
        ({"$bsonSize": 1}, 31393, "$bsonSize requires a document input, found: int"),
        (
            {"$binarySize": 1},
            51276,
            "$binarySize requires a string or BinData argument, found: int",
        ),
        (
            {"$allElementsTrue": 1},
            17040,
            "$allElementsTrue's argument must be an array, but is int",
        ),
        # NOT symmetric with its sibling: mongod spells this one singular.
        # Deriving both from one stem invents `$anyElementsTrue`.
        (
            {"$anyElementTrue": 1},
            17041,
            "$anyElementTrue's argument must be an array, but is int",
        ),
        ({"$concat": [1]}, 16702, "$concat only supports strings, not int"),
        ({"$concatArrays": [1, [2]]}, 28664, "$concatArrays only supports arrays, not int"),
        ({"$in": [1, 2]}, 40081, "$in requires an array as a second argument, found: int"),
        (
            {"$arrayElemAt": [1, 0]},
            28689,
            "$arrayElemAt's first argument must be an array, but is int",
        ),
        (
            {"$slice": [1, 2]},
            28724,
            "First argument to $slice must be an array, but is of type: int",
        ),
        (
            {"$indexOfArray": [1, 2]},
            40090,
            "$indexOfArray requires an array as a first argument, found: int",
        ),
        (
            {"$indexOfBytes": [1, "a"]},
            40091,
            "$indexOfBytes requires a string as the first argument, found: int",
        ),
        # A different code per POSITION, not one code for the operator.
        (
            {"$indexOfBytes": ["ab", 1]},
            40092,
            "$indexOfBytes requires a string as the second argument, found: int",
        ),
        (
            {"$indexOfCP": [1, "a"]},
            40093,
            "$indexOfCP requires a string as the first argument, found: int",
        ),
        (
            {"$indexOfCP": ["ab", 1]},
            40094,
            "$indexOfCP requires a string as the second argument, found: int",
        ),
        (
            {"$split": [1, ","]},
            40085,
            "$split requires an expression that evaluates to a string as a first argument, "
            "found: int",
        ),
        ({"$split": ["a,b", ""]}, 40087, "$split requires a non-empty separator"),
        # A bool is refused here while `$toString` renders it -- these are not
        # the same conversion set.
        ({"$toLower": True}, 16007, "can't convert from BSON type bool to String"),
        ({"$toUpper": True}, 16007, "can't convert from BSON type bool to String"),
    ],
)
def test_a_wrong_typed_operand_names_mongods_error(coll, expr, code, message):
    assert _run(coll, expr) == (code, message)


def test_the_timestamp_operators_keep_mongods_leading_space(coll):
    """`$tsSecond` / `$tsIncrement` open with a space. It is mongod's, not a typo."""
    assert _run(coll, {"$tsSecond": 1}) == (
        5687301,
        " Argument to $tsSecond must be a timestamp, but is int",
    )
    assert _run(coll, {"$tsIncrement": 1}) == (
        5687302,
        " Argument to $tsIncrement must be a timestamp, but is int",
    )


def test_an_absent_field_path_is_missing_not_null(coll):
    """`eval` reports both as null; mongod's message distinguishes them."""
    assert _run(coll, {"$size": "$nosuch"}) == (
        17124,
        "The argument to $size must be an array, but was of type: missing",
    )
    assert _run(coll, {"$size": None}) == (
        17124,
        "The argument to $size must be an array, but was of type: null",
    )


@pytest.mark.parametrize(
    ("expr", "expected"),
    [
        # Null-tolerance is per operator, so it cannot be applied in one place.
        ({"$first": None}, None),
        ({"$last": None}, None),
        ({"$reverseArray": None}, None),
        ({"$tsSecond": None}, None),
        ({"$bsonSize": None}, None),
        ({"$objectToArray": None}, None),
        ({"$arrayToObject": None}, None),
        ({"$binarySize": None}, None),
        ({"$bitAnd": None}, None),
        ({"$concatArrays": [[1], None]}, None),
    ],
)
def test_the_operators_that_answer_null_for_null_still_do(coll, expr, expected):
    assert _run(coll, expr) == ("OK", expected)


@pytest.mark.parametrize(
    ("expr", "code"),
    [
        ({"$size": None}, 17124),
        ({"$strLenCP": None}, 34471),
        ({"$strLenBytes": None}, 34473),
        ({"$allElementsTrue": None}, 17040),
        ({"$anyElementTrue": None}, 17041),
        ({"$in": [1, None]}, 40081),
    ],
)
def test_the_operators_that_refuse_null_still_do(coll, expr, code):
    assert _run(coll, expr)[0] == code


def test_the_bit_operators_use_two_different_shapes(coll):
    """`$bitNot` splits by numeric-ness; the fold operators do not.

    `$bitAnd` answers one sentence for every bad operand, naming no type. Its
    unary sibling names the type -- and uses `28765` for a NON-numeric operand
    against `14` for a numeric one it cannot use.
    """
    assert _run(coll, {"$bitAnd": 1.5}) == (14, "$bitAnd only supports int and long operands.")
    assert _run(coll, {"$bitAnd": "abc"}) == (14, "$bitAnd only supports int and long operands.")
    assert _run(coll, {"$bitNot": 1.5}) == (14, "$bitNot only supports int and long, not: double.")
    assert _run(coll, {"$bitNot": "abc"}) == (
        28765,
        "$bitNot only supports numeric types, not string",
    )


class TestSetOperators:
    """Five operators, three wordings, and two different null rules."""

    @pytest.mark.parametrize("op", ["$setUnion", "$setIntersection", "$setDifference"])
    def test_a_null_operand_makes_the_expression_null(self, coll, op):
        assert _run(coll, {op: [None, [1]]}) == ("OK", None)
        assert _run(coll, {op: [[1], None]}) == ("OK", None)

    def test_operands_are_scanned_left_to_right(self, coll):
        """So the ORDER decides whether the null rule or the type rule fires."""
        assert _run(coll, {"$setUnion": [None, 1]}) == ("OK", None)
        assert _run(coll, {"$setUnion": [1, None]}) == (
            17043,
            "All operands of $setUnion must be arrays. One argument is of type: int",
        )

    def test_setequals_and_setissubset_refuse_null_instead(self, coll):
        assert _run(coll, {"$setEquals": [[1], None]}) == (
            5887502,
            "All operands of $setEquals must be arrays. 2-th argument is of type: null",
        )
        assert _run(coll, {"$setIsSubset": [None, [1]]}) == (
            17046,
            "both operands of $setIsSubset must be arrays. First argument is of type: null",
        )

    def test_setequals_numbers_its_argument_one_based(self, coll):
        """mongod really does write "1-th"."""
        assert _run(coll, {"$setEquals": [1, [2]]}) == (
            5887502,
            "All operands of $setEquals must be arrays. 1-th argument is of type: int",
        )

    def test_the_two_operand_pair_carries_a_code_per_position(self, coll):
        assert _run(coll, {"$setDifference": [1, [2]]})[0] == 17048
        assert _run(coll, {"$setDifference": [[1], 2]})[0] == 17049
        assert _run(coll, {"$setIsSubset": [1, [2]]})[0] == 17046
        assert _run(coll, {"$setIsSubset": [[1], 2]})[0] == 17042


class TestDateExtractors:
    """The thirteen `$year`-family operators share one operand rule."""

    @pytest.mark.parametrize(
        "op",
        [
            "$year",
            "$month",
            "$dayOfMonth",
            "$dayOfWeek",
            "$dayOfYear",
            "$hour",
            "$minute",
            "$second",
            "$millisecond",
            "$week",
            "$isoWeek",
            "$isoWeekYear",
            "$isoDayOfWeek",
        ],
    )
    def test_a_non_date_operand_names_the_type(self, coll, op):
        assert _run(coll, {op: "abc"}) == (
            16006,
            "can't convert from BSON type string to Date",
        )

    def test_a_type_that_carries_a_timestamp_is_accepted(self, coll):
        """An ObjectId's generation time and a Timestamp's seconds ARE dates."""
        assert _run(coll, {"$year": ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")}) == ("OK", 2023)
        assert _run(coll, {"$year": Timestamp(1700000000, 1)}) == ("OK", 2023)

    def test_an_unrecognised_option_outranks_a_missing_date(self, coll):
        """Even when `date` is present and valid, the unknown key is reported."""
        assert _run(coll, {"$year": {"date": WHEN, "k": 1}}) == (
            40535,
            'unrecognized option to $year: "k"',
        )
        assert _run(coll, {"$year": {"k": 1}}) == (
            40535,
            'unrecognized option to $year: "k"',
        )

    def test_the_options_form_requires_a_date(self, coll):
        assert _run(coll, {"$year": {}}) == (
            40539,
            "missing 'date' argument to $year, provided: $year: {}",
        )
        assert _run(coll, {"$year": {"timezone": "UTC"}}) == (
            40539,
            "missing 'date' argument to $year, provided: $year: { timezone: \"UTC\" }",
        )

    def test_a_nested_operator_is_not_the_options_form(self, coll):
        """`{$year: {$add: [1, 2]}}` is an expression, not `{date, timezone}`."""
        assert _run(coll, {"$year": {"$add": [1, 2]}}) == (
            16006,
            "can't convert from BSON type int to Date",
        )


class TestConversions:
    """`$convert` and its eight shorthands."""

    @pytest.mark.parametrize(
        "op",
        [
            "$toBool",
            "$toDate",
            "$toDecimal",
            "$toDouble",
            "$toInt",
            "$toLong",
            "$toObjectId",
            "$toString",
        ],
    )
    def test_an_array_of_any_length_but_one_is_refused(self, coll, op):
        assert _run(coll, {op: []}) == (50723, f"{op} requires a single argument, got 0")
        assert _run(coll, {op: [1, 2]}) == (50723, f"{op} requires a single argument, got 2")

    def test_a_one_element_array_is_the_single_argument(self, coll):
        assert _run(coll, {"$toInt": [1]}) == ("OK", 1)

    def test_an_unsupported_pair_names_both_types(self, coll):
        assert _run(coll, {"$toInt": {"k": 1}}) == (
            241,
            "Unsupported conversion from object to int in $convert with no onError value",
        )
        assert _run(coll, {"$toObjectId": 1}) == (
            241,
            "Unsupported conversion from int to objectId in $convert with no onError value",
        )

    def test_overflow_from_an_integral_source_has_an_EMPTY_tail(self, coll):
        """The message ends on a bare ": ". That is mongod's, not a truncation."""
        assert _run(coll, {"$toInt": Int64(2**40)}) == (
            241,
            "Conversion would overflow target type in $convert with no onError value: ",
        )

    def test_nan_and_infinity_are_named_apart(self, coll):
        assert _run(coll, {"$toInt": float("nan")}) == (
            241,
            "Attempt to convert NaN value to integer type in $convert with no onError value",
        )
        assert _run(coll, {"$toInt": float("inf")}) == (
            241,
            "Attempt to convert infinity value to integer type in $convert with no onError value",
        )

    def test_a_date_converts_to_long_double_and_decimal_but_not_int(self, coll):
        """Epoch milliseconds -- so this cannot be one "numeric" arm."""
        assert _run(coll, {"$toLong": WHEN}) == ("OK", 1767323045000)
        assert _run(coll, {"$toDouble": WHEN}) == ("OK", 1767323045000.0)
        assert _run(coll, {"$toDecimal": WHEN}) == ("OK", Decimal128("1767323045000"))
        assert _run(coll, {"$toInt": WHEN})[0] == 241

    def test_a_date_string_objectid_or_timestamp_converts_to_a_date(self, coll):
        assert _run(coll, {"$toDate": "2026-01-02T03:04:05Z"}) == ("OK", WHEN)
        assert _run(coll, {"$toDate": ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")})[0] == "OK"
        assert _run(coll, {"$toDate": Timestamp(1700000000, 1)})[0] == "OK"

    def test_bool_and_int_are_not_dates(self, coll):
        assert _run(coll, {"$toDate": True}) == (
            241,
            "Unsupported conversion from bool to date in $convert with no onError value",
        )
        assert _run(coll, {"$toDate": 1}) == (
            241,
            "Unsupported conversion from int to date in $convert with no onError value",
        )

    def test_onerror_catches_an_unsupported_pair_too(self, coll):
        """The one form that exists to survive a bad conversion must survive it."""
        expr = {"$convert": {"input": ObjectId(), "to": "int", "onError": "E"}}
        assert _run(coll, expr) == ("OK", "E")


class TestParseTimeRefusals:
    """Errors mongod raises while BUILDING the expression, before it folds."""

    def test_ifnull_and_setequals_word_their_arity_differently(self, coll):
        """One has a comma before "had" and the other does not."""
        assert _run(coll, {"$ifNull": 1}) == (
            1257300,
            "$ifNull needs at least two arguments, had: 1",
        )
        assert _run(coll, {"$setEquals": 1}) == (
            17045,
            "$setEquals needs at least two arguments had: 1",
        )

    def test_setequals_with_an_empty_array_does_not_panic(self, coll):
        """`&arrays[1..]` on a zero-length slice killed the connection thread."""
        assert _run(coll, {"$setEquals": []}) == (
            17045,
            "$setEquals needs at least two arguments had: 0",
        )

    def test_rand_separates_a_bad_shape_from_a_supplied_argument(self, coll):
        """Two codes for what reads as one mistake."""
        assert _run(coll, {"$rand": "x"}) == (
            10065,
            "invalid parameter: expected an object ($rand)",
        )
        assert _run(coll, {"$rand": {"k": 1}}) == (
            3040501,
            "$rand does not currently accept arguments",
        )
        assert _run(coll, {"$rand": [1]}) == (
            3040501,
            "$rand does not currently accept arguments",
        )

    def test_rand_takes_an_empty_document_or_an_empty_array(self, coll):
        for spec in ({}, []):
            code, value = _run(coll, {"$rand": spec})
            assert code == "OK" and 0.0 <= value < 1.0

    def test_getfield_reports_an_unknown_argument(self, coll):
        assert _run(coll, {"$getField": {"k": 1}}) == (
            3041701,
            "$getField found an unknown argument: k",
        )


class TestGetField:
    def test_the_bare_form_is_an_expression_not_a_literal_name(self, coll):
        """A plain string evaluates to itself, so `"s"` still reads field `s`.

        `"$n"` resolves the path and then refuses the int it finds. Taking the
        bare form literally looked for a field NAMED `$n` and answered missing.
        """
        assert _run(coll, {"$getField": "s"}) == ("OK", "abc")
        assert _run(coll, {"$getField": "$n"}) == (
            3041704,
            "$getField requires 'field' to evaluate to type String, but got int",
        )

    def test_a_dollared_name_needs_literal(self, coll):
        coll.insert_one({"_id": 2, "$odd": 7})
        assert _run(coll, {"$getField": {"$literal": "$odd"}})[0] == "OK"

    def test_an_absent_path_is_reported_as_missing(self, coll):
        assert _run(coll, {"$getField": "$nosuch"}) == (
            3041704,
            "$getField requires 'field' to evaluate to type String, but got missing",
        )

    def test_the_object_form_requires_input(self, coll):
        """It does NOT default to the current document the way the bare form does."""
        assert _run(coll, {"$getField": {"field": "s"}}) == (
            3041703,
            "$getField requires 'input' to be specified",
        )

    def test_a_non_string_field_is_3041704(self, coll):
        assert _run(coll, {"$getField": {"field": 5, "input": {}}}) == (
            3041704,
            "$getField requires 'field' to evaluate to type String, but got int",
        )


def test_the_pipeline_wrapper_follows_the_argument(coll):
    """mongod folds a wholly constant expression and says so; one that reads the
    document fails per document under the executor prefix."""

    def wrapper(expr):
        try:
            list(coll.aggregate([{"$addFields": {"z": expr}}]))
        except OperationFailure as e:
            return (e.details or {}).get("errmsg", "").split(" :: caused by :: ")[0]
        raise AssertionError(f"{expr} did not fail")

    assert wrapper({"$size": 1}).startswith("Failed to optimize pipeline")
    assert wrapper({"$size": "$n"}).startswith("Executor error during aggregate")
    # `$getField` reads `$$CURRENT`, so it never folds -- not even here, where
    # both `field` and `input` are literals.
    assert wrapper({"$getField": {"field": 0, "input": {"a": 1}}}).startswith(
        "Executor error during aggregate"
    )
