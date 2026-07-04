"""Postgres range-type support: a range value is stored as a subdocument
``{"lower", "upper", "lower_inc", "upper_inc"}`` (or ``{"empty": True}``); a NULL
bound is unbounded. This module builds, parses, renders, and compares ranges;
``secantus.sql.scalar`` / ``query`` / ``planner`` wire it into the SQL surface.

Supported types: ``int4range`` / ``int8range`` (discrete, canonicalised to the
``[)`` form), ``numrange`` / ``tsrange`` / ``daterange``. Multiranges, custom
range types, and range GiST indexes are out of scope.
"""

from __future__ import annotations

import datetime as _dt
from typing import Any

# type tag -> (element tag, is_discrete). Discrete ranges canonicalise to ``[)``.
RANGE_TYPES: dict[str, tuple[str, bool]] = {
    "int4range": ("int4", True),
    "int8range": ("int8", True),
    "numrange": ("numeric", False),
    "tsrange": ("timestamptz", False),
    "daterange": ("timestamptz", True),
}


def is_range_tag(tag: str | None) -> bool:
    return tag in RANGE_TYPES


class RangeError(ValueError):
    """A malformed range literal / unsupported range operation."""


def _step(tag: str, value: Any) -> Any:
    """The +1 step for a discrete range's canonical ``[)`` form."""
    if tag == "daterange":
        return value + _dt.timedelta(days=1)
    return value + 1


def make_range(lower: Any, upper: Any, bounds: str, tag: str) -> dict[str, Any]:
    """Build a normalised range subdocument from bounds + a ``[)`` / ``(]`` / ``[]``
    / ``()`` spec. Discrete types canonicalise to ``[)``. An empty range (lower ==
    upper with an exclusive side, or lower > upper) collapses to ``{empty: True}``."""
    if len(bounds) != 2 or bounds[0] not in "[(" or bounds[1] not in ")]":
        raise RangeError(f"invalid range bound flags: {bounds!r}")
    lower_inc = bounds[0] == "["
    upper_inc = bounds[1] == "]"
    _elem, discrete = RANGE_TYPES[tag]
    if discrete:
        # Canonicalise to [): a lower exclusive bound steps up; an upper inclusive
        # bound steps up (so [1,10] -> [1,11), (1,10] -> [2,11)).
        if lower is not None and not lower_inc:
            lower = _step(tag, lower)
            lower_inc = True
        if upper is not None and upper_inc:
            upper = _step(tag, upper)
            upper_inc = False
    if (
        lower is not None
        and upper is not None
        and (lower > upper or (lower == upper and not (lower_inc and upper_inc)))
    ):
        return {"empty": True}
    return {"lower": lower, "upper": upper, "lower_inc": lower_inc, "upper_inc": upper_inc}


def is_empty(rng: Any) -> bool:
    return isinstance(rng, dict) and bool(rng.get("empty"))


def lower_bound(rng: Any) -> Any:
    return None if is_empty(rng) else (rng or {}).get("lower")


def upper_bound(rng: Any) -> Any:
    return None if is_empty(rng) else (rng or {}).get("upper")


def contains_value(rng: Any, value: Any) -> bool:
    """Does ``rng`` contain the scalar ``value``?"""
    if value is None or is_empty(rng) or not isinstance(rng, dict):
        return False
    lo, hi = rng.get("lower"), rng.get("upper")
    if lo is not None and (value < lo or (value == lo and not rng.get("lower_inc"))):
        return False
    return not (hi is not None and (value > hi or (value == hi and not rng.get("upper_inc"))))


def _lower_le(a: dict, b: dict) -> bool:
    """Is a's lower bound <= b's lower bound (unbounded lower is smallest)?"""
    la, lb = a.get("lower"), b.get("lower")
    if la is None:
        return True
    if lb is None:
        return False
    if la != lb:
        return la < lb
    return bool(a.get("lower_inc")) or not b.get("lower_inc")


def _upper_ge(a: dict, b: dict) -> bool:
    """Is a's upper bound >= b's upper bound (unbounded upper is largest)?"""
    ua, ub = a.get("upper"), b.get("upper")
    if ua is None:
        return True
    if ub is None:
        return False
    if ua != ub:
        return ua > ub
    return bool(a.get("upper_inc")) or not b.get("upper_inc")


def contains_range(a: Any, b: Any) -> bool:
    """Does range ``a`` contain range ``b``? Every range contains the empty range."""
    if is_empty(b):
        return True
    if is_empty(a) or not isinstance(a, dict) or not isinstance(b, dict):
        return False
    return _lower_le(a, b) and _upper_ge(a, b)


def overlaps(a: Any, b: Any) -> bool:
    """Do ranges ``a`` and ``b`` share at least one point?"""
    if is_empty(a) or is_empty(b) or not isinstance(a, dict) or not isinstance(b, dict):
        return False
    return _after_lower(a, b) and _after_lower(b, a)


def _after_lower(hi_side: dict, lo_side: dict) -> bool:
    """Does ``hi_side``'s upper bound reach ``lo_side``'s lower bound?"""
    up, lo = hi_side.get("upper"), lo_side.get("lower")
    if up is None or lo is None:
        return True
    if up != lo:
        return up > lo
    return bool(hi_side.get("upper_inc")) and bool(lo_side.get("lower_inc"))


def _fmt(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, _dt.datetime):
        return value.isoformat(sep=" ")
    if isinstance(value, _dt.date):
        return value.isoformat()
    return str(value)


def render(rng: Any) -> str:
    """Render a range as the Postgres text form ``[lower,upper)`` (``empty`` for the
    empty range)."""
    if is_empty(rng) or not isinstance(rng, dict):
        return "empty"
    lb = "[" if rng.get("lower_inc") else "("
    ub = "]" if rng.get("upper_inc") else ")"
    return f"{lb}{_fmt(rng.get('lower'))},{_fmt(rng.get('upper'))}{ub}"


def parse_literal(text: str, tag: str, coerce: Any) -> dict[str, Any]:
    """Parse a range text literal (``[1,10)`` / ``(1,10]`` / ``empty``) into a
    normalised subdocument. ``coerce`` converts a bound token to the element type."""
    s = text.strip()
    if s.lower() == "empty":
        return {"empty": True}
    if len(s) < 2 or s[0] not in "[(" or s[-1] not in ")]":
        raise RangeError(f"malformed range literal: {text!r}")
    bounds = s[0] + s[-1]
    body = s[1:-1]
    # Split on the first top-level comma (bounds are scalar tokens here).
    if "," not in body:
        raise RangeError(f"malformed range literal: {text!r}")
    lo_s, hi_s = body.split(",", 1)
    lo = coerce(lo_s.strip().strip('"')) if lo_s.strip() else None
    hi = coerce(hi_s.strip().strip('"')) if hi_s.strip() else None
    return make_range(lo, hi, bounds, tag)
