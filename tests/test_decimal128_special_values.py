"""``Decimal128`` NaN / ±Infinity through the math operators.

These are the special values, and they carry no precision — so an operator can
answer them with ordinary ``float`` arithmetic and hand back a ``Decimal128``,
with none of the 34-digit decimal math a FINITE decimal would need. That is why
they are separable from the rest of the family, which still defers on the Rust
server (``tasks/backlog.md``).

Every expectation below is mongod 8.2.11's own answer, generated from a live
server on 2026-09-07 rather than written by hand. Four of them defeat a guess:

* ``$ceil`` / ``$floor`` of a Decimal **infinity** are ``NaN``, not the infinity;
* ``$ln`` / ``$log10`` of a Decimal **NaN** come back as a **double** ``nan`` —
  the one place in this family where the argument's type is not kept;
* ``$cosh(-Infinity)`` is ``+Infinity``;
* ``$abs(-0)`` is ``0`` while ``$trunc(-0)`` is ``-0``.

The Python engine reached this file wrong in five places: its domain guards
tested ``isinstance(v, (int, float))``, so a ``Decimal128`` slipped past them
into the decimal path and returned ``NaN`` where mongod raises — ``$sqrt`` and
``$ln`` and ``$log10`` of a Decimal ``-Infinity``. The Rust engine simply
deferred, which on the Rust server is an error. **The parity suite was green
throughout**, because neither engine's corpus contained these shapes: parity
pins the two engines to each other and is equally satisfied by both being
wrong.
"""

from __future__ import annotations

import math

import pytest
from bson import Decimal128

from secantus.expressions import ExpressionError, evaluate

_secantus_core = pytest.importorskip(
    "_secantus_core", reason="the Rust engine is only built with the `rust` extra"
)

NAN = float("nan")


class Err:
    """mongod raises, with this code."""

    def __init__(self, code: int) -> None:
        self.code = code

    def __repr__(self) -> str:
        return f"Err({self.code})"


#: (operator, argument, mongod's answer) — generated from mongod 8.2.11.
CASES: list[tuple[str, Decimal128, object]] = [
    ("$sqrt", Decimal128("NaN"), Decimal128("NaN")),
    ("$sqrt", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$sqrt", Decimal128("-Infinity"), Err(28714)),
    ("$exp", Decimal128("NaN"), Decimal128("NaN")),
    ("$exp", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$exp", Decimal128("-Infinity"), Decimal128("0")),
    ("$ln", Decimal128("NaN"), NAN),
    ("$ln", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$ln", Decimal128("-Infinity"), Err(28766)),
    ("$log10", Decimal128("NaN"), NAN),
    ("$log10", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$log10", Decimal128("-Infinity"), Err(28761)),
    ("$degreesToRadians", Decimal128("NaN"), Decimal128("NaN")),
    ("$degreesToRadians", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$degreesToRadians", Decimal128("-Infinity"), Decimal128("-Infinity")),
    ("$radiansToDegrees", Decimal128("NaN"), Decimal128("NaN")),
    ("$radiansToDegrees", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$radiansToDegrees", Decimal128("-Infinity"), Decimal128("-Infinity")),
    ("$sin", Decimal128("NaN"), Decimal128("NaN")),
    ("$sin", Decimal128("Infinity"), Err(50989)),
    ("$sin", Decimal128("-Infinity"), Err(50989)),
    ("$cos", Decimal128("NaN"), Decimal128("NaN")),
    ("$cos", Decimal128("Infinity"), Err(50989)),
    ("$cos", Decimal128("-Infinity"), Err(50989)),
    ("$tan", Decimal128("NaN"), Decimal128("NaN")),
    ("$tan", Decimal128("Infinity"), Err(50989)),
    ("$tan", Decimal128("-Infinity"), Err(50989)),
    ("$asin", Decimal128("NaN"), Decimal128("NaN")),
    ("$asin", Decimal128("Infinity"), Err(50989)),
    ("$asin", Decimal128("-Infinity"), Err(50989)),
    ("$acos", Decimal128("NaN"), Decimal128("NaN")),
    ("$acos", Decimal128("Infinity"), Err(50989)),
    ("$acos", Decimal128("-Infinity"), Err(50989)),
    ("$atan", Decimal128("NaN"), Decimal128("NaN")),
    ("$atan", Decimal128("Infinity"), Decimal128("1.570796326794896619231321691639751")),
    ("$atan", Decimal128("-Infinity"), Decimal128("-1.570796326794896619231321691639751")),
    ("$sinh", Decimal128("NaN"), Decimal128("NaN")),
    ("$sinh", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$sinh", Decimal128("-Infinity"), Decimal128("-Infinity")),
    ("$cosh", Decimal128("NaN"), Decimal128("NaN")),
    ("$cosh", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$cosh", Decimal128("-Infinity"), Decimal128("Infinity")),
    ("$tanh", Decimal128("NaN"), Decimal128("NaN")),
    ("$tanh", Decimal128("Infinity"), Decimal128("1")),
    ("$tanh", Decimal128("-Infinity"), Decimal128("-1")),
    ("$asinh", Decimal128("NaN"), Decimal128("NaN")),
    ("$asinh", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$asinh", Decimal128("-Infinity"), Decimal128("-Infinity")),
    ("$acosh", Decimal128("NaN"), Decimal128("NaN")),
    ("$acosh", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$acosh", Decimal128("-Infinity"), Err(50989)),
    ("$atanh", Decimal128("NaN"), Decimal128("NaN")),
    ("$atanh", Decimal128("Infinity"), Err(50989)),
    ("$atanh", Decimal128("-Infinity"), Err(50989)),
    ("$abs", Decimal128("NaN"), Decimal128("NaN")),
    ("$abs", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$abs", Decimal128("-Infinity"), Decimal128("Infinity")),
    ("$trunc", Decimal128("NaN"), Decimal128("NaN")),
    ("$trunc", Decimal128("Infinity"), Decimal128("Infinity")),
    ("$trunc", Decimal128("-Infinity"), Decimal128("-Infinity")),
    ("$ceil", Decimal128("NaN"), Decimal128("NaN")),
    ("$ceil", Decimal128("Infinity"), Decimal128("NaN")),
    ("$ceil", Decimal128("-Infinity"), Decimal128("NaN")),
    ("$floor", Decimal128("NaN"), Decimal128("NaN")),
    ("$floor", Decimal128("Infinity"), Decimal128("NaN")),
    ("$floor", Decimal128("-Infinity"), Decimal128("NaN")),
]


def _ids() -> list[str]:
    return [f"{op[1:]}-{arg}" for op, arg, _ in CASES]


def _same(actual: object, expected: object) -> bool:
    if isinstance(expected, float) and math.isnan(expected):
        return isinstance(actual, float) and math.isnan(actual)
    if isinstance(expected, Decimal128):
        return isinstance(actual, Decimal128) and str(actual) == str(expected)
    return type(actual) is type(expected) and actual == expected


@pytest.mark.parametrize("op,arg,expected", CASES, ids=_ids())
def test_python_engine_matches_mongod(op: str, arg: Decimal128, expected: object) -> None:
    if isinstance(expected, Err):
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: arg}, {"_id": 1})
        assert exc.value.code == expected.code
        return
    assert _same(evaluate({op: arg}, {"_id": 1}), expected)


@pytest.mark.parametrize("op,arg,expected", CASES, ids=_ids())
def test_rust_engine_matches_mongod(op: str, arg: Decimal128, expected: object) -> None:
    """The Rust engine must ANSWER these, not defer.

    A defer is a fallback only in the Python server; the Rust server has no
    Python behind it, so a defer there surfaces as
    `2 BadValue: ... not supported by the Rust server`.
    """
    import bson

    raw = _secantus_core.evaluate(
        bson.encode({"_id": 1}), bson.encode({"e": {op: arg}}), bson.encode({})
    )
    assert raw is not None, f"the Rust engine DEFERRED on {op} {arg}"
    wrapped = bson.decode(raw)
    if isinstance(expected, Err):
        assert "err" in wrapped, f"expected {expected} for {op} {arg}, got {wrapped}"
        assert wrapped["err"]["code"] == expected.code
        return
    assert "err" not in wrapped, f"the Rust engine raised on {op} {arg}: {wrapped}"
    assert _same(wrapped.get("r"), expected)


def test_a_finite_decimal_still_takes_the_precise_path() -> None:
    """The special-value shortcut must not swallow a FINITE decimal, whose
    answer carries real precision. mongod: 34 significant digits."""
    out = evaluate({"$sqrt": Decimal128("2.5")}, {"_id": 1})
    assert isinstance(out, Decimal128)
    assert str(out).startswith("1.5811388300841896")


# --------------------------------------------------------------------------
# The DOMAIN checks, which apply by VALUE and so cover a decimal too.
#
# These were the wrong-ANSWER half: `isinstance(v, (int, float))` guards let
# every negative or zero decimal through into the decimal path, so
# `$ln(Decimal128("-1"))` returned `NaN` and `$ln(Decimal128("0"))` returned
# `-Infinity` where mongod refuses both. Measured against 8.2.11, 2026-09-07.
# --------------------------------------------------------------------------

DOMAIN_CASES: list[tuple[str, str, object]] = [
    # $sqrt allows zero of either sign and refuses anything below it.
    ("$sqrt", "-0", Decimal128("-0")),
    ("$sqrt", "0", Decimal128("0")),
    ("$sqrt", "-1", Err(28714)),
    ("$sqrt", "-2.5", Err(28714)),
    ("$sqrt", "-0.5", Err(28714)),
    # $ln / $log10 refuse zero as well -- `-0` included.
    ("$ln", "-0", Err(28766)),
    ("$ln", "0", Err(28766)),
    ("$ln", "-1", Err(28766)),
    ("$ln", "-2.5", Err(28766)),
    ("$log10", "-0", Err(28761)),
    ("$log10", "0", Err(28761)),
    ("$log10", "-1", Err(28761)),
    ("$log10", "-2.5", Err(28761)),
]


@pytest.mark.parametrize(
    "op,literal,expected",
    DOMAIN_CASES,
    ids=[f"{op[1:]}-{lit}" for op, lit, _ in DOMAIN_CASES],
)
def test_domain_checks_cover_decimals_python(op: str, literal: str, expected: object) -> None:
    arg = Decimal128(literal)
    if isinstance(expected, Err):
        with pytest.raises(ExpressionError) as exc:
            evaluate({op: arg}, {"_id": 1})
        assert exc.value.code == expected.code
        return
    assert _same(evaluate({op: arg}, {"_id": 1}), expected)


@pytest.mark.parametrize(
    "op,literal,expected",
    [c for c in DOMAIN_CASES if isinstance(c[2], Err)],
    ids=[f"{op[1:]}-{lit}" for op, lit, e in DOMAIN_CASES if isinstance(e, Err)],
)
def test_domain_checks_cover_decimals_rust(op: str, literal: str, expected: object) -> None:
    """The Rust engine must RAISE these rather than defer — the error is
    knowable from the value alone, with no decimal math involved."""
    import bson

    raw = _secantus_core.evaluate(
        bson.encode({"_id": 1}),
        bson.encode({"e": {op: Decimal128(literal)}}),
        bson.encode({}),
    )
    assert raw is not None, f"the Rust engine DEFERRED on {op} {literal}"
    wrapped = bson.decode(raw)
    assert "err" in wrapped, f"expected {expected} for {op} {literal}, got {wrapped}"
    assert wrapped["err"]["code"] == expected.code


def test_the_error_message_renders_the_decimal_as_a_double() -> None:
    """mongod renders the operand in its VALUE form, so `Decimal128("-2.5")`
    reads `-2.5` and `Decimal128("-0")` reads `-0`."""
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$ln": Decimal128("-0")}, {"_id": 1})
    assert str(exc.value) == "$ln's argument must be a positive number, but is -0"
    with pytest.raises(ExpressionError) as exc:
        evaluate({"$log10": Decimal128("-2.5")}, {"_id": 1})
    assert str(exc.value) == "$log10's argument must be a positive number, but is -2.5"
