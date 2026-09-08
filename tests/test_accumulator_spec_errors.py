"""Spec-shape errors for the accumulator-style operators, in BOTH positions.

`$firstN`, `$lastN`, `$minN`, `$maxN`, `$median`, `$percentile`, `$topN` and
`$bottomN` take a document spec. When they are given something else, **mongod's
code depends on where the operator appears** — a `$group` output field is
ACCUMULATOR position and an `$addFields` value is EXPRESSION position, and the
two do not agree:

    {$group:     {x: {$median: 5}}}   ->  7436100
    {$addFields: {x: {$median: 5}}}   ->  7436201

Both servers applied the expression codes in both positions, which was right for
four of the eight and wrong for four. An ARRAY spec in accumulator position is a
different message again — one shared by all eight — and `$topN` / `$bottomN`
are accumulator-only, so as an expression mongod does not recognise the name at
all.

Measured across all eight operators × {scalar, array} × {accumulator,
expression} against mongod 8.2.11 on 2026-09-08: **16 of 32 cells diverged**,
now 0.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer
from secantus.aggregate import expression_problem_in_pipeline


@pytest.fixture(scope="module")
def coll(tmp_path_factory):
    """A real server, because the `$median` / `$percentile` spec errors are
    raised while EVALUATING the accumulator, not by the parse-time walker --
    asserting them against the walker tests the wrong layer and reports a
    failure the wire never sees."""
    home = tmp_path_factory.mktemp("accspec") / "wt"
    with SecantusDBServer(port=0, storage_path=str(home)) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        c = client["accspec"]["c"]
        c.insert_many([{"_id": i, "v": i} for i in range(3)])
        try:
            yield c
        finally:
            client.close()


def group_error(coll, spec):
    """`(code, message)` for a `$group` accumulator spec, over the wire."""
    with pytest.raises(OperationFailure) as excinfo:
        list(coll.aggregate([{"$group": {"_id": None, "x": spec}}]))
    err = excinfo.value
    return err.code, err.details.get("errmsg", "").split(":: caused by :: ")[-1]


#: (operator, accumulator-position code, expression-position code or None when
#: the operator is not an expression at all).
OPERATORS = [
    ("$firstN", 5787801, 5787801),
    ("$lastN", 5787801, 5787801),
    ("$minN", 5787900, 5787900),
    ("$maxN", 5787900, 5787900),
    ("$median", 7436100, 7436201),
    ("$percentile", 7429703, 7436200),
    ("$topN", 5788001, None),
    ("$bottomN", 5788001, None),
]


def problem(stage):
    return expression_problem_in_pipeline([stage], frozenset())


@pytest.mark.parametrize("op,acc_code,_e", OPERATORS, ids=[o for o, _, _ in OPERATORS])
def test_scalar_spec_in_accumulator_position(op, acc_code, _e):
    code, message, _stage = problem({"$group": {"_id": None, "x": {op: 5}}})
    assert code == acc_code
    assert message == f"specification must be an object; found {op}: 5"


@pytest.mark.parametrize("op,_a,expr_code", OPERATORS, ids=[o for o, _, _ in OPERATORS])
def test_scalar_spec_in_expression_position(op, _a, expr_code):
    code, message, _stage = problem({"$addFields": {"x": {op: 5}}})
    if expr_code is None:
        # Accumulator-only: the name is not an expression, so the spec is never
        # examined. Code 168 is shared with the object-spec complaint, which is
        # why asserting only the code hid this for as long as it did.
        assert (code, message) == (168, f"Unrecognized expression '{op}'")
    else:
        assert code == expr_code
        assert message == f"specification must be an object; found {op}: 5"


@pytest.mark.parametrize("op,_a,_e", OPERATORS, ids=[o for o, _, _ in OPERATORS])
def test_array_spec_in_accumulator_position_is_one_message_for_all_eight(op, _a, _e):
    code, message, _stage = problem({"$group": {"_id": None, "x": {op: [1, 2]}}})
    assert (code, message) == (40237, f"The {op} accumulator is a unary operator")


def test_id_is_an_expression_not_an_accumulator():
    """`_id` is the group KEY, so it takes expression-position codes."""
    code, message, _stage = problem({"$group": {"_id": {"$median": 5}}})
    assert code == 7436201
    assert message == "specification must be an object; found $median: 5"


# ---------------------------------------------------------------------------
# $median / $percentile spec validation, in mongod's IDL field order
# ---------------------------------------------------------------------------

MISSING = [
    # `input` is declared first, so it is named before `method` -- `{}` is an
    # `input` complaint, not a `method` one.
    ({"$median": {}}, 40414, "BSON field '$median.input' is missing but a required field"),
    (
        {"$percentile": {}},
        40414,
        "BSON field '$percentile.input' is missing but a required field",
    ),
    (
        {"$median": {"input": "$v"}},
        40414,
        "BSON field '$median.method' is missing but a required field",
    ),
    (
        {"$median": {"method": "approximate"}},
        40414,
        "BSON field '$median.input' is missing but a required field",
    ),
    (
        {"$percentile": {"input": "$v", "method": "approximate"}},
        40414,
        "BSON field '$percentile.p' is missing but a required field",
    ),
    # `method` is declared LAST, so a bad `method` loses to a bad `p`.
    (
        {"$percentile": {"input": "$v", "method": "exact", "p": "x"}},
        7750301,
        "The $percentile 'p' field must be an array of numbers from [0.0, 1.0], but found: \"x\"",
    ),
    (
        {"$median": {"input": "$v", "method": "exact"}},
        2,
        "Currently only 'approximate' can be used as a percentile 'method'.",
    ),
    # An unknown field outranks every one of them.
    (
        {"$median": {"zz": 1, "method": "approximate"}},
        40415,
        "BSON field '$median.zz' is an unknown field.",
    ),
    (
        {"$median": {"input": "$v", "p": [0.5], "method": "approximate"}},
        40415,
        "BSON field '$median.p' is an unknown field.",
    ),
]


@pytest.mark.parametrize("spec,code,message", MISSING, ids=[str(s)[:40] for s, _, _ in MISSING])
def test_percentile_spec_validation_order(coll, spec, code, message):
    assert group_error(coll, spec) == (code, message)


P_VALUES = [
    # A non-array and an EMPTY array share 7750301 and name the array itself.
    (0.5, 7750301, "0.5"),
    ({}, 7750301, "{}"),
    ([], 7750301, "[]"),
    # A non-number ELEMENT is 7750302; a number out of range is 7750303.
    (["a"], 7750302, '"a"'),
    ([None], 7750302, "null"),
    ([1.5], 7750303, "1.5"),
    ([-0.1], 7750303, "-0.1"),
]


@pytest.mark.parametrize("p,code,rendered", P_VALUES, ids=[str(p) for p, _, _ in P_VALUES])
def test_percentile_p_field_has_three_distinct_codes(coll, p, code, rendered):
    spec = {"$percentile": {"input": "$v", "p": p, "method": "approximate"}}
    assert group_error(coll, spec) == (
        code,
        f"The $percentile 'p' field must be an array of numbers from [0.0, 1.0], "
        f"but found: {rendered}",
    )


@pytest.mark.parametrize("p", [[0], [1], [0.5], [0.5, 0.9]])
def test_percentile_accepts_the_closed_unit_interval(coll, p):
    spec = {"$percentile": {"input": "$v", "p": p, "method": "approximate"}}
    got = list(coll.aggregate([{"$group": {"_id": None, "x": spec}}]))
    assert len(got[0]["x"]) == len(p)
