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
from collections.abc import Callable
from typing import Any

from bson import Int64
from bson.decimal128 import Decimal128

_INT32_MIN = -(2**31)
_INT32_MAX = 2**31 - 1


def _as_decimal(v: Any) -> decimal.Decimal:
    if isinstance(v, Decimal128):
        return v.to_decimal()
    if isinstance(v, float):
        # ``Decimal(str(float))`` avoids the binary-float artifacts of
        # ``Decimal(float)`` (e.g. 0.1 → 0.1000000000000000055…).
        return decimal.Decimal(str(v))
    return decimal.Decimal(int(v))


def _combine(a: Any, b: Any, op: Callable[[Any, Any], Any]) -> Any:
    # Decimal dominates the widening order.
    if isinstance(a, Decimal128) or isinstance(b, Decimal128):
        return Decimal128(op(_as_decimal(a), _as_decimal(b)))
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
