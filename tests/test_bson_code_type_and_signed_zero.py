"""Two vocabularies the pure engine got wrong, both probed against mongod 8.2.11.

* `bson.Code` subclasses `str`, so anything that dispatches on
  ``isinstance(v, str)`` classifies a JavaScript value as a string. That root
  cause has produced a documented run of bugs; this file pins the two surfaces
  that still had it -- the ``$type`` operator and the shared type-name helper,
  which did not know ``javascriptWithScope`` at all.
* IEEE keeps the SIGN when a rounding lands on zero. ``math.ceil`` returns an
  ``int``, which has no ``-0.0``, so ``$ceil`` / ``$floor`` / ``$trunc`` dropped
  it where mongod keeps it.

Every expectation below is a MEASURED mongod value, not a derived one: the
`$type` vocabulary and the error-message vocabulary were probed independently
across 21 value classes and agree, and the rounding cases were probed across
-0.0 / -0.5 / -1.5 for each operator (2026-09-03).
"""

from __future__ import annotations

import math
import re

import pytest
from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp

from secantus.bsontypes import bson_type_name
from secantus.expressions import evaluate

# (value, the type name mongod reports on BOTH the `$type` and error surfaces)
TYPE_NAMES = [
    (None, "null"),
    (True, "bool"),
    (5, "int"),
    (Int64(5), "long"),
    (1.5, "double"),
    (Decimal128("2.5"), "decimal"),
    ("x", "string"),
    (Code("x=1"), "javascript"),
    (Code("x=1", {}), "javascriptWithScope"),
    (Binary(b"z"), "binData"),
    (b"z", "binData"),
    (ObjectId("64b7f9a2c1d2e3f4a5b6c7d8"), "objectId"),
    (Timestamp(1, 1), "timestamp"),
    (Regex("a", "i"), "regex"),
    (re.compile("a"), "regex"),
    (MinKey(), "minKey"),
    (MaxKey(), "maxKey"),
    ([1], "array"),
    ({"k": 1}, "object"),
]


@pytest.mark.parametrize(("value", "expected"), TYPE_NAMES)
def test_type_operator_names_the_bson_type(value, expected):
    """`$type` reported `"string"` for JavaScript and `"object"` for a compiled
    pattern: its `_type_name` was a fourth partial copy of the vocabulary."""
    assert evaluate({"$type": {"$literal": value}}, {}) == expected


@pytest.mark.parametrize(("value", "expected"), TYPE_NAMES)
def test_the_shared_helper_names_the_same_type(value, expected):
    """One vocabulary, not two. `bson_type_name` feeds error messages and now
    `$type` as well, so a drift between them is a failure here."""
    assert bson_type_name(value) == expected


def test_a_scoped_code_is_a_different_bson_type_from_a_bare_one():
    """Type 15 vs type 13. The helper called both `javascript`, so every error
    message about a scoped Code named the wrong type."""
    assert bson_type_name(Code("x=1")) == "javascript"
    assert bson_type_name(Code("x=1", {})) == "javascriptWithScope"
    assert bson_type_name(Code("x=1", None)) == "javascript"


def _is_negative_zero(v):
    return isinstance(v, float) and v == 0.0 and math.copysign(1.0, v) < 0.0


@pytest.mark.parametrize(
    ("op", "value", "expected", "negative_zero"),
    [
        # A rounding that lands on zero KEEPS the sign.
        ("$ceil", -0.0, 0.0, True),
        ("$ceil", -0.5, 0.0, True),
        ("$trunc", -0.0, 0.0, True),
        ("$trunc", -0.5, 0.0, True),
        ("$floor", -0.0, 0.0, True),
        ("$round", -0.5, 0.0, True),
        # ...and every other magnitude is unaffected.
        ("$ceil", 0.5, 1.0, False),
        ("$ceil", -1.5, -1.0, False),
        ("$floor", -0.5, -1.0, False),
        ("$floor", 0.5, 0.0, False),
        ("$trunc", -1.5, -1.0, False),
        ("$trunc", 1.5, 1.0, False),
        # `$abs` of -0.0 is POSITIVE zero on mongod -- the one that must not
        # come through the sign-preserving path.
        ("$abs", -0.0, 0.0, False),
    ],
)
def test_rounding_preserves_ieee_signed_zero(op, value, expected, negative_zero):
    got = evaluate({op: {"$literal": value}}, {})
    assert got == expected, f"{op} of {value!r} -> {got!r}"
    assert _is_negative_zero(got) is negative_zero, f"{op} of {value!r} -> {got!r}"


@pytest.mark.parametrize("op", ["$ceil", "$floor", "$trunc", "$round"])
def test_rounding_stays_type_preserving(op):
    """The signed-zero fix must not change the BSON type: an int stays an int."""
    assert evaluate({op: {"$literal": 3}}, {}) == 3
    assert not isinstance(evaluate({op: {"$literal": 3}}, {}), float)
