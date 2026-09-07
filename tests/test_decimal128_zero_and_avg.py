"""A `Decimal128` ZERO through the math operators, and `$avg`'s arithmetic.

Two families that both looked like "needs 34-digit decimal math" and are not.

**The decimal zeros are CONSTANTS.** Every one of them, so no series has to
run — which is what separates them from the finite decimals in the same
operators, which genuinely do need decimal transcendentals and still defer on
the Rust server. The per-operator QUANTUM is load-bearing and unguessable
(`$tan` answers `0E-40`, `$asinh` answers `0E-6176`, `$cos` writes 1 to 34
places), and so is the sign rule: the ODD functions carry `-0` through and the
EVEN ones drop it. The table below was GENERATED from mongod 8.2.11 on
2026-09-07, not transcribed.

Both engines were wrong here, differently. The Rust engine deferred (an error on
the Rust server). The Python engine ran its decimal series and produced a bare
`0` where mongod carries a quantum, a bare `1` where mongod writes 34 digits,
and dropped the sign of `-0` entirely.

**`$avg` was a wrong ANSWER on the Python server.** mongod converts the integer
total to a double and THEN divides; it does not do exact integer division. The
two agree until the total passes 2**53 and then they do not::

    $avg: [2**53+1, 2**53+3, 2**53+5]
        mongod  9007199254740994.0     (float(sum) / n)
        before  9007199254740996.0     (sum / n, correctly rounded)

Python's `int / int` is correctly rounded over the exact quotient — a BETTER
answer, and the wrong one, because the conformance target is mongod's
arithmetic. The Rust engine deferred above 2**53 with a comment reading "defer
to Python int/int divide", so it was deferring TO that wrong answer: a comment
justifying behaviour by the other engine rather than by the oracle, which is
the shape CLAUDE.md warns about.
"""

from __future__ import annotations

import pytest
from bson import Decimal128, Int64

from secantus.expressions import ExpressionError, evaluate

_secantus_core = pytest.importorskip(
    "_secantus_core", reason="the Rust engine is only built with the `rust` extra"
)


class Err:
    """mongod raises, with this code."""

    def __init__(self, code: int) -> None:
        self.code = code

    def __repr__(self) -> str:
        return f"Err({self.code})"


#: (operator, decimal literal, mongod's answer) — generated from mongod 8.2.11.
ZERO_CASES: list[tuple[str, str, object]] = [
    ("$sin", "0", Decimal128("0")),
    ("$sin", "-0", Decimal128("-0")),
    ("$cos", "0", Decimal128("1.000000000000000000000000000000000")),
    ("$cos", "-0", Decimal128("1.000000000000000000000000000000000")),
    ("$tan", "0", Decimal128("0E-40")),
    ("$tan", "-0", Decimal128("-0E-40")),
    ("$asin", "0", Decimal128("0E-40")),
    ("$asin", "-0", Decimal128("-0E-40")),
    ("$acos", "0", Decimal128("1.570796326794896619231321691639751")),
    ("$acos", "-0", Decimal128("1.570796326794896619231321691639751")),
    ("$atan", "0", Decimal128("0")),
    ("$atan", "-0", Decimal128("-0")),
    ("$sinh", "0", Decimal128("0E-40")),
    ("$sinh", "-0", Decimal128("-0E-40")),
    ("$cosh", "0", Decimal128("1.000000000000000000000000000000000")),
    ("$cosh", "-0", Decimal128("1.000000000000000000000000000000000")),
    ("$tanh", "0", Decimal128("0")),
    ("$tanh", "-0", Decimal128("-0")),
    ("$asinh", "0", Decimal128("0E-6176")),
    ("$asinh", "-0", Decimal128("-0E-6176")),
    ("$atanh", "0", Decimal128("0E-6176")),
    ("$atanh", "-0", Decimal128("-0E-6176")),
    ("$exp", "0", Decimal128("1")),
    ("$exp", "-0", Decimal128("1")),
    ("$sqrt", "0", Decimal128("0")),
    ("$sqrt", "-0", Decimal128("-0")),
    ("$degreesToRadians", "0", Decimal128("0E-35")),
    ("$degreesToRadians", "-0", Decimal128("-0E-35")),
    ("$radiansToDegrees", "0", Decimal128("0E-32")),
    ("$radiansToDegrees", "-0", Decimal128("-0E-32")),
    ("$abs", "0", Decimal128("0")),
    ("$abs", "-0", Decimal128("0")),
    ("$trunc", "0", Decimal128("0")),
    ("$trunc", "-0", Decimal128("-0")),
    ("$ceil", "0", Decimal128("0")),
    ("$ceil", "-0", Decimal128("-0")),
    ("$floor", "0", Decimal128("0")),
    ("$floor", "-0", Decimal128("-0")),
]


def _ids() -> list[str]:
    return [f"{op[1:]}-{lit}" for op, lit, _ in ZERO_CASES]


def _same(actual: object, expected: object) -> bool:
    if isinstance(expected, Decimal128):
        # `Decimal128.__eq__` compares VALUES, so `0` == `0E-40` and the whole
        # point of these cases would be lost. The quantum and the sign both
        # live in the text.
        return isinstance(actual, Decimal128) and str(actual) == str(expected)
    return type(actual) is type(expected) and actual == expected


@pytest.mark.parametrize("op,literal,expected", ZERO_CASES, ids=_ids())
def test_python_engine_matches_mongod(op: str, literal: str, expected: object) -> None:
    arg = Decimal128(literal)
    if isinstance(expected, Err):
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: arg}, {"_id": 1})
        assert exc.value.code == expected.code
        return
    assert _same(evaluate({op: arg}, {"_id": 1}), expected)


@pytest.mark.parametrize("op,literal,expected", ZERO_CASES, ids=_ids())
def test_rust_engine_matches_mongod(op: str, literal: str, expected: object) -> None:
    """The Rust engine must ANSWER these rather than defer — a defer has no
    Python behind it on the Rust server."""
    import bson

    raw = _secantus_core.evaluate(
        bson.encode({"_id": 1}),
        bson.encode({"e": {op: Decimal128(literal)}}),
        bson.encode({}),
    )
    assert raw is not None, f"the Rust engine DEFERRED on {op} {literal}"
    wrapped = bson.decode(raw)
    if isinstance(expected, Err):
        assert "err" in wrapped, f"expected {expected} for {op} {literal}, got {wrapped}"
        assert wrapped["err"]["code"] == expected.code
        return
    assert "err" not in wrapped, f"the Rust engine raised on {op} {literal}: {wrapped}"
    assert _same(wrapped.get("r"), expected)


def test_the_quantum_is_what_these_cases_are_about() -> None:
    """A guard against someone "simplifying" the table to bare zeros.

    `Decimal128("0") == Decimal128("0E-40")` compares equal, so a test that
    used `==` would pass on the wrong answer. These assert the TEXT.
    """
    assert str(evaluate({"$tan": Decimal128("0")}, {"_id": 1})) == "0E-40"
    assert str(evaluate({"$asinh": Decimal128("0")}, {"_id": 1})) == "0E-6176"
    assert str(evaluate({"$cos": Decimal128("0")}, {"_id": 1})) == (
        "1.000000000000000000000000000000000"
    )
    # ...and the sign of a negative zero survives the ODD functions only.
    assert str(evaluate({"$sin": Decimal128("-0")}, {"_id": 1})) == "-0"
    assert str(evaluate({"$cos": Decimal128("-0")}, {"_id": 1})).startswith("1.0")


#: (values, mongod's average). mongod converts the integer TOTAL to a double
#: and then divides — generated from 8.2.11, 2026-09-07.
AVG_CASES: list[tuple[list[int], float]] = [
    ([2**53 + 1, 2**53 + 3, 2**53 + 5], 9007199254740994.0),
    ([9007199254740993, 1, 1], 3002399751580332.0),
    ([2**63 - 1], 9.223372036854776e18),
    ([2**62, 2**62 - 1, 2**62 - 3], 4.611686018427388e18),
    ([10], 10.0),
    ([1, 2], 1.5),
]


@pytest.mark.parametrize("values,expected", AVG_CASES, ids=[str(v) for v, _ in AVG_CASES])
def test_avg_divides_the_way_mongod_does(values: list[int], expected: float) -> None:
    out = evaluate({"$avg": [Int64(v) for v in values]}, {"_id": 1})
    assert isinstance(out, float)
    assert out == expected


@pytest.mark.parametrize("values,expected", AVG_CASES, ids=[str(v) for v, _ in AVG_CASES])
def test_rust_avg_matches(values: list[int], expected: float) -> None:
    import bson

    raw = _secantus_core.evaluate(
        bson.encode({"_id": 1}),
        bson.encode({"e": {"$avg": [Int64(v) for v in values]}}),
        bson.encode({}),
    )
    assert raw is not None, f"the Rust engine DEFERRED on $avg {values}"
    assert bson.decode(raw).get("r") == expected


def test_avg_above_2_to_the_53_is_not_the_exact_quotient() -> None:
    """The case that separates mongod's arithmetic from the accurate one."""
    values = [2**53 + 1, 2**53 + 3, 2**53 + 5]
    exact = sum(values) / len(values)
    assert exact == 9007199254740996.0, "the exact quotient, which mongod does NOT give"
    assert evaluate({"$avg": [Int64(v) for v in values]}, {"_id": 1}) == 9007199254740994.0
