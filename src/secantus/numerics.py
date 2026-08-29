"""MongoDB BSON numeric type promotion for arithmetic.

MongoDB preserves the BSON numeric *type* of an arithmetic result following a
widening order — int32 < int64 < double < decimal128. So ``$inc`` / ``$mul`` /
``$sum`` and friends must return an ``Int64`` (which BSON-encodes as a 64-bit
int) — not a bare Python ``int`` that encodes as int32 — when any operand is a
64-bit int, a ``float`` when any operand is a double, and a ``Decimal128`` when
any operand is decimal. Plain Python arithmetic loses this distinction because
``int + Int64`` returns the base ``int`` class, so the long-ness is dropped and
a small result silently narrows to int32 on the wire.

These helpers reproduce mongod's promotion rules so client codecs that key on
the BSON subtype (e.g. an Int64-only type decoder) round-trip correctly.
"""

from __future__ import annotations

import decimal
import math
from collections.abc import Callable
from typing import Any

from bson import Int64
from bson.decimal128 import Decimal128

_INT32_MIN = -(2**31)
_INT32_MAX = 2**31 - 1

#: Decimal128 carries 34 significant digits, but Python's *default* decimal
#: context is 28 — so arithmetic on stored decimals silently truncated six
#: digits off every result (``Decimal128("1.000000000000000000000000000000001")
#: + 1`` answered ``2.000000000000000000000000000`` where mongod keeps the
#: trailing 1). Every combine runs in this context instead. ROUND_HALF_EVEN is
#: both Python's default and IEEE 754-2008's for decimal128, so a product that
#: genuinely exceeds 34 digits rounds the way mongod rounds it.
_DECIMAL128_CTX = decimal.Context(prec=34, rounding=decimal.ROUND_HALF_EVEN)


#: mongod converts a double to decimal128 at a fixed 15 significant digits.
_DOUBLE_SIG_DIGITS = 15

#: decimal128's coefficient width.
_DECIMAL128_DIGITS = 34


def decimal_from_double(v: float) -> decimal.Decimal:
    """A double as decimal, the way mongod converts one.

    mongod is fixed at 15 significant digits (probed 6.0.16): ``3.0`` becomes
    ``3.00000000000000``, not ``3.0``. The padding is not cosmetic — decimal128
    keeps its quantum, so the conversion decides the quantum of every mixed
    double/decimal result (``Decimal128("1.5") + 3.0`` is ``4.50000000000000``)
    and of ``$toDecimal`` output alike.

    Rounding runs on the double's **exact** binary value rather than its
    shortest repr. The two agree for ordinary magnitudes but not at the
    denormal edge, where mongod answers ``5e-324`` with
    ``4.94065645841247E-324`` — the exact value to 15 digits, not ``5.00…E-324``.
    """
    d = decimal.Decimal(v)  # exact, not repr-shortened
    if not d.is_finite():
        return d
    if not d:
        # mongod renders a zero double as plain `0` / `-0`, unpadded.
        return decimal.Decimal("-0") if math.copysign(1.0, v) < 0 else decimal.Decimal(0)
    with decimal.localcontext() as ctx:
        ctx.prec = _DOUBLE_SIG_DIGITS + 10
        return d.quantize(decimal.Decimal(1).scaleb(d.adjusted() - (_DOUBLE_SIG_DIGITS - 1)))


def decimal_from_double_exact(v: float) -> decimal.Decimal:
    """A double as decimal the way mongod's **accumulators** convert one.

    `$sum` / `$avg` do *not* use the 15-digit rule that `$inc` / `$mul` /
    `$toDecimal` use — they take the double's exact binary value, capped at
    decimal128's 34 digits (probed 6.0.16). The two rules give visibly
    different answers for the same operands::

        $inc  by 0.1  ->  0.100000000000000                    (15 digits)
        $sum  of 0.1  ->  0.1000000000000000055511151231257827 (exact, 34)

    A double that *is* exact keeps its short form either way, so
    ``$sum`` of ``3.0`` is ``3`` rather than a padded ``3.00000000000000``.
    """
    d = decimal.Decimal(v)  # exact binary value; Decimal(3.0) is Decimal("3")
    if not d.is_finite():
        return d
    with decimal.localcontext() as ctx:
        # decimal128 can only hold 34 digits, so mongod rounds on the way in —
        # rounding here rather than at the end reproduces that single step.
        ctx.prec = _DECIMAL128_DIGITS
        return +d


def _as_decimal(
    v: Any, conv: Callable[[float], decimal.Decimal] = decimal_from_double
) -> decimal.Decimal:
    if isinstance(v, Decimal128):
        return v.to_decimal()
    if isinstance(v, float):
        return conv(v)
    # Integers convert exactly — mongod applies no digit limit to them.
    return decimal.Decimal(int(v))


def _combine(a: Any, b: Any, op: Callable[[Any, Any], Any], *, exact_double: bool = False) -> Any:
    # Decimal dominates the widening order.
    if isinstance(a, Decimal128) or isinstance(b, Decimal128):
        conv = decimal_from_double_exact if exact_double else decimal_from_double
        with decimal.localcontext(_DECIMAL128_CTX):
            return Decimal128(op(_as_decimal(a, conv), _as_decimal(b, conv)))
    # Then double.
    if isinstance(a, float) or isinstance(b, float):
        return op(float(a), float(b))
    # Integral domain: the result is int64 if either operand is already a
    # 64-bit int, or if a 32-bit result would overflow — otherwise int32.
    res = op(int(a), int(b))
    if isinstance(a, Int64) or isinstance(b, Int64) or not (_INT32_MIN <= res <= _INT32_MAX):
        return Int64(res)
    return int(res)


def bson_add(a: Any, b: Any) -> Any:
    """``a + b`` with MongoDB BSON numeric type promotion."""
    return _combine(a, b, lambda x, y: x + y)


def bson_mul(a: Any, b: Any) -> Any:
    """``a * b`` with MongoDB BSON numeric type promotion."""
    return _combine(a, b, lambda x, y: x * y)


def bson_sum(a: Any, b: Any) -> Any:
    """``a + b`` for the `$sum` / `$avg` **accumulators**.

    Identical to :func:`bson_add` except for how a double joins the decimal
    domain: accumulators take its exact binary value where the update
    operators take 15 significant digits. See
    :func:`decimal_from_double_exact`.
    """
    return _combine(a, b, lambda x, y: x + y, exact_double=True)
