"""`$ln`, `$log10`, `$exp` and `$asinh` on a finite Decimal128, on BOTH engines.

These four used to refuse a finite non-zero decimal outright — a `BadValue` on
the Rust server, where a deferral has no Python behind it. They now compute at
decimal128's 34 significant digits.

**The reference here is the correctly-rounded value, not mongod's answer**, and
that is a deliberate choice. Over 290 measured pairs mongod is correctly rounded
on 231: it carries Intel RDFP's approximation error in the last digit on the
rest, and reproducing that would mean linking RDFP itself. So on roughly a fifth
of finite inputs these operators differ from mongod 8.2.11 by one unit in the
last place — `$ln(Decimal128("2.5"))` is `…117680111` here and `…117680110`
there, and the true value is `…1176801107145…`. See `tasks/backlog.md`.

Rather than hard-code expectations that could be mistyped, each test computes
its own reference with the stdlib `decimal` module at 250 digits and rounds it
to 34 significant digits. `test_reference_is_stable` is the self-check: it
re-derives a known value at three precisions and fails the run if they disagree,
so a broken reference cannot quietly pass everything.

Values are compared NUMERICALLY. A differing quantum (`1` versus
`1.000000000000000000000000000000000`) is the same decimal128 value, and mongod
is itself inconsistent about it — `$log10` of `1E+9` is `9` and of `1E+10` is
`10.00000000000000000000000000000000`.
"""

from __future__ import annotations

from decimal import ROUND_HALF_EVEN, Context, Decimal

import pytest
from bson import Decimal128

from secantus.expressions import evaluate

REF = Context(prec=250)


def ref(op: str, value: str) -> Decimal:
    """The correctly-rounded 34-significant-digit value of `op(value)`."""
    d = REF.create_decimal(value)
    with_ctx = {
        "$ln": lambda x: x.ln(REF),
        "$log10": lambda x: x.log10(REF),
        "$exp": lambda x: x.exp(REF),
        "$asinh": lambda x: REF.minus(_asinh(x)) if x < 0 else _asinh(x),
    }[op](d)
    if with_ctx == 0:
        return with_ctx
    quantum = Decimal(1).scaleb(with_ctx.adjusted() - 33)
    # NOT `+x`: unary plus applies the thread-local context, whose default
    # precision is 28, and it silently cut the reference to 28 digits.
    return with_ctx.quantize(quantum, rounding=ROUND_HALF_EVEN, context=REF)


def _asinh(x: Decimal) -> Decimal:
    a = abs(x)
    return REF.ln(REF.add(a, REF.sqrt(REF.add(REF.multiply(a, a), Decimal(1)))))


def engine(op: str, value: str):
    return evaluate({op: {"$literal": Decimal128(value)}}, {})


def test_reference_is_stable():
    """The self-check: a reference that disagrees with itself proves nothing.

    It covers the ROUNDING too, not just the series -- the first version checked
    only `Context.ln` and passed while `ref()` was quietly returning 28 digits,
    which failed 47 tests that were in fact correct.
    """
    for prec in (60, 120, 250):
        c = Context(prec=prec)
        got = c.ln(c.create_decimal("2.5"))
        assert str(got).startswith("0.91629073187415506518352721176801107145")
    # And `ref` itself, against values with 34 digits written out by hand.
    assert str(ref("$ln", "2.5")) == "0.9162907318741550651835272117680111"
    assert str(ref("$asinh", "10")) == "2.998222950297969738846595537596453"
    assert str(ref("$exp", "1")) == "2.718281828459045235360287471352662"
    assert str(ref("$log10", "2")) == "0.3010299956639811952137388947244930"


LN = ["1", "2", "10", "2.5", "0.5", "100", "1E+400", "1E-400", "7.125", "0.001", "1.5", "3"]
LOG10 = ["1", "10", "100", "1E+400", "0.001", "2", "2.5", "7.125", "0.5", "1E+9", "1E+34"]
EXP = ["0", "1", "2.5", "-1", "180", "10", "-10", "0.5", "4920.26", "2275.28", "-100", "9215.59"]
ASINH = [
    "1",
    "-1",
    "2",
    "2.5",
    "10",
    "0.5",
    "0.1",
    "100",
    "3",
    "0.001",
    "1E+10",
    "1E-10",
    "1E-15",
    "1E-17",
    "1E-100",
    # `1E-3000` is deliberately absent: the 250-digit reference cannot reach
    # it either (`1 + 1E-3000` is `1` there), so it is pinned by
    # `test_asinh_of_a_tiny_argument_keeps_every_digit` instead.
    "-1E-100",
    "1E+34",
    "1E+400",
    "1E+6144",
    "-1E+6144",
    "0.9",
    "1.1",
    "-2.5",
]


@pytest.mark.parametrize("value", LN)
def test_ln_is_correctly_rounded(value):
    assert engine("$ln", value).to_decimal() == ref("$ln", value)


@pytest.mark.parametrize("value", LOG10)
def test_log10_is_correctly_rounded(value):
    assert engine("$log10", value).to_decimal() == ref("$log10", value)


@pytest.mark.parametrize("value", EXP)
def test_exp_is_correctly_rounded(value):
    assert engine("$exp", value).to_decimal() == ref("$exp", value)


@pytest.mark.parametrize("value", ASINH)
def test_asinh_is_correctly_rounded(value):
    assert engine("$asinh", value).to_decimal() == ref("$asinh", value)


def test_asinh_of_a_tiny_argument_keeps_every_digit():
    """`asinh(x) = x - x^3/6 + ...`, so far below 1 the answer IS `x`.

    Computing `ln(x + sqrt(x^2+1))` at any fixed precision loses this: `1 + x`
    rounds to `1` and the answer collapses to zero. Both engines returned a
    wrong value here — Rust `0`, Python eleven digits of error at `1E-10`.
    """
    assert str(engine("$asinh", "1E-100")) == "1.000000000000000000000000000000000E-100"
    assert str(engine("$asinh", "-1E-100")) == "-1.000000000000000000000000000000000E-100"
    assert str(engine("$asinh", "1E-3000")) == "1.000000000000000000000000000000000E-3000"
    # Just above the shortcut, where the series must still run.
    assert str(engine("$asinh", "1E-15")) == "9.999999999999999999999999999998333E-16"


def test_asinh_follows_mongods_underflow_to_zero():
    """Below `1E-4966` mongod's own implementation underflows to a bare `0`.

    Not `0E-6176` -- that is what it answers for an exact zero -- and not the
    mathematically correct value, which is what this returned when it was
    written to agree with the other engine rather than with mongod. The
    threshold was bisected against 8.2.11 on 2026-09-08.

    mongod's answers just ABOVE the threshold are progressively wrong too
    (`$asinh(1E-4965)` is `1.295…E-4965` where the true value is `1E-4965`);
    that band is a breakdown in its implementation and is not reproduced here.
    """
    assert str(engine("$asinh", "1E-6176")) == "0"
    assert str(engine("$asinh", "-1E-6176")) == "-0"
    assert str(engine("$asinh", "1E-6100")) == "0"
    assert str(engine("$asinh", "1E-4966")) == "0"
    # Still the value one exponent above the threshold.
    assert str(engine("$asinh", "1E-4000")) == "1.000000000000000000000000000000000E-4000"


@pytest.mark.parametrize(
    "op,value,expected",
    [
        # Exact powers of ten answer the integer with no series at all, which is
        # the only way `$log10` of a power of ten comes out exact.
        ("$log10", "1", "0"),
        ("$log10", "1E+400", "400"),
        ("$log10", "0.001", "-3"),
        # Zeros and the specials, measured from mongod 8.2.11.
        ("$asinh", "0", "0E-6176"),
        ("$asinh", "-0", "-0E-6176"),
        ("$asinh", "Infinity", "Infinity"),
        ("$asinh", "-Infinity", "-Infinity"),
        ("$asinh", "NaN", "NaN"),
        ("$exp", "0", "1"),
        ("$exp", "Infinity", "Infinity"),
        ("$exp", "-Infinity", "0"),
        ("$ln", "1", "0"),
        ("$ln", "Infinity", "Infinity"),
    ],
)
def test_exact_and_special_cases(op, value, expected):
    assert str(engine(op, value)) == expected


@pytest.mark.parametrize("op,code", [("$ln", 28766), ("$log10", 28761)])
@pytest.mark.parametrize("value", ["0", "-0", "-1", "-1E-6176", "-Infinity"])
def test_non_positive_is_a_domain_error(op, code, value):
    from secantus.expressions import ExpressionError

    with pytest.raises(ExpressionError) as e:
        engine(op, value)
    assert e.value.code == code
