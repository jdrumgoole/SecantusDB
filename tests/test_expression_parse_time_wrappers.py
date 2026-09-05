"""Which expression errors are PARSE errors, and so carry the stage's wrapper.

mongod uses three wrappers for a failing expression in an `$addFields` /
`$project` / `$set`:

* `Invalid $addFields :: caused by ::` — a PARSE error, raised while building
  the expression tree, before anything is folded;
* `Failed to optimize pipeline :: caused by ::` — a constant-fold failure;
* `Executor error during aggregate command on namespace: … :: caused by ::` —
  a runtime failure, per document.

Arity and spec-shape problems are parse errors. Eighty-three shapes carried the
wrong one of these three while their message BODY was already byte-identical to
mongod's, so nothing but the prefix was wrong — and a comparison that looked at
the code alone could not see it.

Every code and wording below is measured mongod output (8.2.11, 2026-09-05),
including two inconsistencies that must be reproduced rather than tidied:
`$ifNull` puts a comma before `had:` and `$setEquals` does not.
"""

from __future__ import annotations

import pytest

from secantus.aggregate import _expression_shape_problem

STAGE = "Invalid $addFields :: caused by :: "


@pytest.mark.parametrize(
    ("spec", "code", "message"),
    [
        # Per-operator minimums: their own code, their own wording.
        ({"$ifNull": [1]}, 1257300, "$ifNull needs at least two arguments, had: 1"),
        ({"$ifNull": []}, 1257300, "$ifNull needs at least two arguments, had: 0"),
        ({"$ifNull": "x"}, 1257300, "$ifNull needs at least two arguments, had: 1"),
        ({"$setEquals": [[1]]}, 17045, "$setEquals needs at least two arguments had: 1"),
        ({"$setEquals": []}, 17045, "$setEquals needs at least two arguments had: 0"),
        # Required keys in a spec document.
        ({"$convert": {"to": "int"}}, 9, "Missing 'input' parameter to $convert"),
        ({"$dateDiff": {"unit": "day"}}, 5166303, "Missing 'startDate' parameter to $dateDiff"),
        ({"$firstN": {"input": [1]}}, 5787906, "Missing value for 'n'"),
        ({"$lastN": {"input": [1]}}, 5787906, "Missing value for 'n'"),
        ({"$maxN": {"input": [1]}}, 5787906, "Missing value for 'n'"),
        ({"$minN": {"input": [1]}}, 5787906, "Missing value for 'n'"),
        (
            {"$dateFromParts": {"month": 1}},
            40516,
            "$dateFromParts requires either 'year' or 'isoWeekYear' to be present",
        ),
    ],
)
def test_parse_time_problems_are_detected_before_folding(spec, code, message):
    """`_expression_shape_problem` returning non-None is what selects the STAGE
    wrapper; returning None sends the error down the fold path instead."""
    assert _expression_shape_problem(spec) == (code, message)


@pytest.mark.parametrize(
    ("spec", "code"),
    [
        # An UNRECOGNISED key is reported before a MISSING required one.
        ({"$dateDiff": {"k": 1}}, 5166302),
        ({"$dateFromParts": {"k": 1}}, 40518),
        ({"$firstN": {"k": 1}}, 5787901),
        ({"$lastN": {"k": 1}}, 5787901),
        ({"$maxN": {"k": 1}}, 5787901),
        ({"$minN": {"k": 1}}, 5787901),
    ],
)
def test_an_unknown_key_is_reported_before_a_missing_one(spec, code):
    """The ordering rule, and the reason it has its own test.

    Adding the missing-key checks AHEAD of the unknown-key ones changed the code
    on exactly these six shapes, which were already correct. Both checks fire on
    the same spec document, so only their order distinguishes them — and the
    probe reports it as a code regression, not a message one.
    """
    got = _expression_shape_problem(spec)
    assert got is not None and got[0] == code, got


def test_a_valid_spec_has_no_parse_time_problem():
    """The guard against over-matching: these must fall through to folding."""
    for spec in (
        {"$ifNull": ["$a", 1]},
        {"$setEquals": [[1], [1]]},
        {"$convert": {"input": "$a", "to": "int"}},
        {"$firstN": {"input": [1], "n": 1}},
        {"$dateFromParts": {"year": 2026}},
        {"$dateDiff": {"startDate": "$a", "endDate": "$b", "unit": "day"}},
    ):
        assert _expression_shape_problem(spec) is None, spec


# --- `$rand` -------------------------------------------------------------
#
# 45 shapes, one operator: every wrong argument to `$rand` already produced the
# right code and the right sentence, and carried the EXECUTOR wrapper where
# mongod uses the stage's. Both ways of getting it wrong are parse errors.


@pytest.mark.parametrize(
    ("arg", "expected"),
    [
        # An EMPTY document or array is the no-argument call and is VALID.
        # `{$rand: []}` being accepted is the easy one to miss.
        ({}, None),
        ([], None),
        # A non-empty one of either: "does not currently accept arguments".
        ({"x": 1}, (3040501, "$rand does not currently accept arguments")),
        ([1], (3040501, "$rand does not currently accept arguments")),
        ({"$literal": 1}, (3040501, "$rand does not currently accept arguments")),
        # A SCALAR is a different complaint with a different code -- two codes
        # for what reads as one mistake, which is why this is not a single
        # "must be a document" check.
        (0, (10065, "invalid parameter: expected an object ($rand)")),
        (1, (10065, "invalid parameter: expected an object ($rand)")),
        ("s", (10065, "invalid parameter: expected an object ($rand)")),
        (None, (10065, "invalid parameter: expected an object ($rand)")),
        (True, (10065, "invalid parameter: expected an object ($rand)")),
    ],
)
def test_rand_argument_problems_are_parse_errors(arg, expected):
    """Measured against mongod 8.2.11 (2026-09-05) across all ten shapes."""
    assert _expression_shape_problem({"$rand": arg}) == expected
