"""Exact ``numeric`` for the Python PostgreSQL server.

A Postgres ``numeric`` holds up to 131072 integer digits and 16383 fraction
digits, exactly, with its display scale (``1.50`` is not ``1.5`` on screen,
though the two are equal). BSON's Decimal128 holds 34 significant digits and
exponents ``-6176 .. 6111``, and this server used to store every numeric as
one -- so a wider value silently ROUNDED, and a smaller exponent was CLAMPED to
a different number (``1E-7000`` was stored as ``1E-6176``).

This is a port of the Rust server's representation
(``crates/secantus-pgplan/src/numeric.rs``), so both servers store a numeric
the same way:

* a value a Decimal128 holds EXACTLY -- digits and display scale -- is stored
  as that Decimal128, which is what a Mongo client reading the collection
  expects and what the overwhelming majority of values are;
* anything else is stored as ``{"__numeric": <canonical text>, "__numkey":
  <sort key>}`` in the column itself. There is no companion field beside the
  column, so there is nothing to keep in sync on the write paths.

``__numkey`` is a string whose BYTEWISE order is the numeric order (`sort_key`),
which is what lets a ``WHERE`` on a numeric column be lowered to a Mongo filter
that is exact for rows of either form (`filter_for`).

"Holds exactly" is judged by rendering the Decimal128 back, NOT by whether its
constructor raises: pymongo's ``Decimal128`` raises ``Inexact`` for a 35th
digit, but silently drops trailing zeros (``1.000…0`` with 42 fraction digits
keeps 33) and clamps an exponent (``0E-7000`` becomes ``0E-6176``). Measured.
"""

from __future__ import annotations

import decimal
from decimal import Decimal
from typing import Any

import bson

from secantus.sql import errors

#: Marker key carrying the canonical text of a numeric too wide for Decimal128.
WIDE_KEY = "__numeric"
#: Marker key carrying the byte-sortable key of a wide numeric.
SORT_KEY = "__numkey"

#: Postgres' own limits (probed on 16 by the Rust port): `1e131071` is the
#: widest integer and `1e-16383` the smallest scale that parse.
MAX_INT_DIGITS = 131072
MAX_SCALE = 16383

#: An arithmetic context that never rounds a value Postgres can hold. Python's
#: DEFAULT context rounds at 28 digits -- below even Decimal128's 34 -- so
#: ``+`` / ``-`` / ``*`` on a numeric must run under this one.
EXACT = decimal.Context(
    prec=MAX_INT_DIGITS + MAX_SCALE + 2,
    Emax=decimal.MAX_EMAX,
    Emin=decimal.MIN_EMIN,
    traps=[decimal.InvalidOperation, decimal.DivisionByZero],
)

_NAN = bson.Decimal128("NaN")


def _overflow() -> errors.SQLError:
    return errors.SQLError("22003", "value overflows numeric format")


def canonical(d: Decimal) -> str:
    """Postgres' text for ``d``: plain notation, display scale kept, no ``-0``,
    and ``NaN`` / ``Infinity`` / ``-Infinity`` spelled as Postgres spells them."""
    if d.is_nan():
        return "NaN"
    if d.is_infinite():
        return "-Infinity" if d.is_signed() else "Infinity"
    text = format(d, "f")
    if d.is_zero() and text.startswith("-"):
        text = text[1:]
    return text


def check_limits(d: Decimal) -> Decimal:
    """``d``, or 22003 when it is outside what a Postgres numeric can hold."""
    if not d.is_finite():
        return d
    sign, digits, exp = d.as_tuple()
    assert isinstance(exp, int)
    int_digits = max(0, len(digits) + exp) if not d.is_zero() else 0
    scale = max(0, -exp)
    if int_digits > MAX_INT_DIGITS or scale > MAX_SCALE:
        raise _overflow()
    return d


def _exact_decimal128(d: Decimal) -> bson.Decimal128 | None:
    try:
        candidate = bson.Decimal128(d)
    except (decimal.DecimalException, ValueError):
        return None
    return candidate if canonical(candidate.to_decimal()) == canonical(d) else None


def stored(d: Decimal) -> Any:
    """The BSON value a ``numeric`` column stores for ``d``: its Decimal128 when
    one holds it exactly, else the wide-numeric document."""
    if not d.is_finite():
        return bson.Decimal128(d)  # NaN / Infinity always fit, and never widen
    check_limits(d)
    exact = _exact_decimal128(d)
    if exact is not None:
        return exact
    text = canonical(d)
    return {WIDE_KEY: text, SORT_KEY: sort_key(text)}


def is_wide(v: Any) -> bool:
    """Whether ``v`` is the wide-numeric document."""
    return isinstance(v, dict) and WIDE_KEY in v


def to_decimal(v: Any) -> Decimal | None:
    """A stored numeric of either form as an exact ``Decimal``; else None."""
    if isinstance(v, bson.Decimal128):
        return v.to_decimal()
    if isinstance(v, Decimal):
        return v
    if is_wide(v):
        return Decimal(v[WIDE_KEY])
    return None


def sort_key(text: str) -> str:
    """A string whose BYTEWISE order is the numeric order of canonical texts,
    NaN above everything as Postgres has it.

    Layout (identical to the Rust server's): a class byte (``0`` -Infinity,
    ``1`` negative, ``2`` zero, ``3`` positive, ``4`` Infinity, ``5`` NaN); for
    a non-zero finite value, the exponent ``E`` of ``0.d1d2… × 10^E`` as a
    7-digit offset decimal, then the significant digits, trailing zeros
    removed. A negative value's exponent and digits are nines-complemented and
    terminated by ``~``, so a larger-magnitude negative sorts first. Scale is
    not part of the key: ``1.50`` and ``1.5`` share one."""
    if text == "NaN":
        return "5"
    if text == "Infinity":
        return "4"
    if text == "-Infinity":
        return "0"
    neg = text.startswith("-")
    body = text[1:] if neg else text
    int_part, _, frac = body.partition(".")
    int_part = int_part.lstrip("0")
    digits = int_part + frac
    if not digits.strip("0"):
        return "2"
    leading = len(digits) - len(digits.lstrip("0"))
    exponent = len(int_part) - leading
    significant = digits[leading:].rstrip("0")
    exp_field = f"{exponent + 1_000_000:07d}"
    if neg:
        comp = str.maketrans("0123456789", "9876543210")
        return "1" + exp_field.translate(comp) + significant.translate(comp) + "~"
    return "3" + exp_field + significant


def order_key(d: Decimal) -> tuple:
    """A Python sort key giving Postgres' TOTAL order: NaN equal to itself and
    above every number, infinity included. (``Decimal('NaN') < x`` raises
    ``InvalidOperation``, so a bare Decimal cannot be sorted when a NaN is
    present.)"""
    if d.is_nan():
        return (1, Decimal(0))
    return (0, d)


def _bracket(d: Decimal) -> tuple[bool, bson.Decimal128, bson.Decimal128]:
    """Where ``d`` sits on the Decimal128 number line: ``(True, x, x)`` when a
    Decimal128 has the same VALUE (display scale aside), else ``(False, below,
    above)``, its nearest neighbours. Every stored Decimal128 compares with
    ``d`` exactly as it compares with the bracket, which is what lets a wide
    constant be lowered to MQL."""
    if not d.is_finite():
        return True, bson.Decimal128(d), bson.Decimal128(d)
    if d.is_zero():
        zero = bson.Decimal128("0")
        return True, zero, zero
    normal = d.normalize(EXACT)
    try:
        return True, bson.Decimal128(normal), bson.Decimal128(normal)
    except (decimal.DecimalException, ValueError):
        pass
    # The Decimal128 grid around |d|: at most 34 significant digits, and no
    # exponent below -6176. Snapping to THAT grid gives the true neighbours.
    # (The Rust original truncates to 34 digits and, when the exponent then
    # falls below -6176, falls back to (0, 1E-6176) -- which does not contain a
    # value like 1.2...E-6150, so a range filter against it selected wrong
    # Decimal128 rows. Found by this module's invariant check.)
    sign = normal.is_signed()
    mag = normal.copy_abs()
    step_exp = max(mag.adjusted() - 33, -6176)
    if step_exp > 6111:
        below = bson.Decimal128("9999999999999999999999999999999999E+6111")
        above = bson.Decimal128("Infinity")
    else:
        step = Decimal((0, (1,), step_exp))
        lo = mag.quantize(step, rounding=decimal.ROUND_FLOOR, context=EXACT)
        hi = EXACT.add(lo, step)
        below, above = bson.Decimal128(lo), bson.Decimal128(hi)
    if sign:
        return (
            False,
            # copy_negate, not unary minus: `-x` rounds in the default
            # 28-digit context, and these bounds carry 34 digits.
            bson.Decimal128(above.to_decimal().copy_negate()),
            bson.Decimal128(below.to_decimal().copy_negate()),
        )
    return False, below, above


def filter_for(field: str, op: str, value: Decimal) -> dict[str, Any]:
    """``field <op> value`` as a Mongo filter exact for a column holding
    numerics of either form. ``op`` is ``$eq`` / ``$ne`` / ``$gt`` / ``$gte``
    / ``$lt`` / ``$lte``.

    The narrow arm compares the stored Decimal128 against the constant's
    bracket; the wide arm compares ``<field>.__numkey`` against the constant's
    own key. A Decimal128 row has no ``__numkey``, so it never satisfies the
    wide arm of a range or equality and always satisfies its ``$ne`` -- which
    is exactly the arm it should take.

    NaN is lowered by hand: Postgres puts it equal to itself and ABOVE every
    number, where Mongo's range operators exclude it. So ``> x`` / ``>= x``
    pick up the NaN rows as a third arm. A NaN is always a Decimal128."""
    wide_field = f"{field}.{SORT_KEY}"
    if value.is_nan():
        not_nan_not_null = {"$and": [{field: {"$ne": _NAN}}, {field: {"$ne": None}}]}
        return {
            "$eq": {field: _NAN},
            "$gte": {field: _NAN},
            "$gt": {field: {"$in": []}},
            "$ne": not_nan_not_null,
            "$lt": not_nan_not_null,
            "$lte": {field: {"$ne": None}},
        }[op]
    key = sort_key(canonical(value))
    exact, lo, hi = _bracket(value)
    if op == "$ne":
        arms: list[dict[str, Any]] = []
        if exact:
            arms.append({field: {"$ne": lo}})
        arms.append({wide_field: {"$ne": key}})
        arms.append({field: {"$ne": None}})
        return {"$and": arms}
    narrow: dict[str, Any] | None
    if op == "$eq":
        narrow = {field: lo} if exact else None
    elif op == "$gt":
        narrow = {field: {"$gt": lo}} if exact else {field: {"$gte": hi}}
    elif op == "$gte":
        narrow = {field: {"$gte": lo if exact else hi}}
    elif op == "$lt":
        narrow = {field: {"$lt": lo}} if exact else {field: {"$lte": lo}}
    elif op == "$lte":
        narrow = {field: {"$lte": lo}}
    else:
        raise ValueError(f"unsupported numeric filter operator {op!r}")
    arms = [a for a in (narrow, {wide_field: {op: key}}) if a is not None]
    if op in ("$gt", "$gte"):
        arms.append({field: _NAN})
    return arms[0] if len(arms) == 1 else {"$or": arms}


class SortKey:
    """An ORDER BY / window key for a numeric, in Postgres' TOTAL order: NaN
    equal to itself and above every number. Compares against another SortKey
    or a plain int / float / Decimal, so a numeric column sorts beside integer
    expression results. (A bare ``Decimal('NaN')`` raises on ``<``, which made
    ``ORDER BY`` over a column holding NaN an internal error.)"""

    __slots__ = ("d",)

    def __init__(self, d: Decimal) -> None:
        self.d = d

    @staticmethod
    def _key(v: Any) -> tuple:
        if isinstance(v, SortKey):
            v = v.d
        if isinstance(v, float) and v != v:
            return (1, Decimal(0))
        if isinstance(v, Decimal):
            return order_key(v)
        return (0, Decimal(v) if isinstance(v, (int, float)) else v)

    def __eq__(self, other: object) -> bool:
        try:
            return self._key(self) == self._key(other)
        except (TypeError, decimal.InvalidOperation):
            return NotImplemented

    def __lt__(self, other: Any) -> bool:
        return self._key(self) < self._key(other)

    def __gt__(self, other: Any) -> bool:
        return self._key(self) > self._key(other)

    def __le__(self, other: Any) -> bool:
        return self._key(self) <= self._key(other)

    def __ge__(self, other: Any) -> bool:
        return self._key(self) >= self._key(other)

    def __hash__(self) -> int:
        return hash(self._key(self))

    def __repr__(self) -> str:
        return f"SortKey({self.d!r})"


#: Marker keys for an exact numeric aggregate. Mongo's `$sum` skips a wide
#: document and rounds a Decimal128 total at 34 digits, and `$min` / `$max`
#: order a document above every number -- so over a numeric column the planner
#: `$push`es ``{<marker>: value}`` instead and the executor folds the list here,
#: exactly. Recognised by VALUE (like the sub-millisecond composites), so every
#: accumulator site gets it without registering anything.
AGG_MARKERS = {"sum": "__numsum", "min": "__nummin", "max": "__nummax"}
_MARKER_FUNC = {v: k for k, v in AGG_MARKERS.items()}


def marked_func(value: Any) -> str | None:
    """The aggregate a pushed marker list belongs to, or None if it is not one."""
    if not isinstance(value, list) or not value:
        return None
    first = value[0]
    if not isinstance(first, dict) or len(first) != 1:
        return None
    return _MARKER_FUNC.get(next(iter(first)))


def fold(func: str, values: list[Any]) -> Decimal | None:
    """Finish ``sum`` / ``min`` / ``max`` over pushed marker documents.

    NULLs (and a FILTER's non-matching rows, which push NULL) contribute
    nothing; an all-NULL group is NULL, as in Postgres. The sum is exact and
    keeps the widest input scale (``Decimal`` addition does)."""
    marker = AGG_MARKERS[func]
    nums: list[Decimal] = []
    for item in values:
        v = item.get(marker) if isinstance(item, dict) else None
        if v is None:
            continue
        d = to_decimal(v)
        if d is None and isinstance(v, (int, float)) and not isinstance(v, bool):
            d = Decimal(v)
        if d is not None:
            nums.append(d)
    if not nums:
        return None
    if func == "sum":
        total = Decimal(0)
        for d in nums:
            total = EXACT.add(total, d)
        return total
    pick = min if func == "min" else max
    return pick(nums, key=order_key)


_NAN_EQ_KEY = ("\x00nan",)


def eq_key(value: Any) -> Any:
    """A hashable key under which two SQL values collide exactly when
    Postgres treats them as the same row value (DISTINCT, UNION / INTERSECT /
    EXCEPT, DISTINCT ON, PARTITION BY).

    Those paths keyed on ``repr()``, and ``repr(Decimal("1.5"))`` is not
    ``repr(Decimal("1.50"))``: ``select 1.5 union select 1.50`` returned two
    rows where Postgres returns one, INTERSECT of the pair returned nothing,
    and ``-0.0`` / ``0.0`` split the same way. Numbers compare by value here
    (Python's ``int`` / ``float`` / ``Decimal`` already hash and compare that
    way), every NaN is one value (Postgres' NaN equals NaN), a bool is not a
    number, and arrays compare element by element. Anything else keeps its
    ``repr`` identity.
    """
    if isinstance(value, bool) or value is None:
        return value
    if isinstance(value, (int, float, Decimal)):
        if value != value:  # float NaN, Decimal NaN
            return _NAN_EQ_KEY
        return ("\x00num", value)
    if isinstance(value, bson.Decimal128):
        return eq_key(value.to_decimal())
    if isinstance(value, (list, tuple)):
        return ("\x00arr", tuple(eq_key(v) for v in value))
    return repr(value)
