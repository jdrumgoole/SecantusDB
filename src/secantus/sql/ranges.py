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

import bson

# type tag -> (element tag, is_discrete). Discrete ranges canonicalise to ``[)``.
RANGE_TYPES: dict[str, tuple[str, bool]] = {
    "int4range": ("int4", True),
    "int8range": ("int8", True),
    "numrange": ("numeric", False),
    "tsrange": ("timestamptz", False),
    "tstzrange": ("timestamptz", False),
    "daterange": ("timestamptz", True),
}


def _cv(v: Any) -> Any:
    """A bound value made comparable: ``bson.Decimal128`` (a ``numrange`` bound's
    storage form) has no ordering operators, so unwrap it to ``Decimal``."""
    return v.to_decimal() if isinstance(v, bson.Decimal128) else v


def is_range_tag(tag: str | None) -> bool:
    return tag in RANGE_TYPES


class RangeError(ValueError):
    """A malformed range literal / unsupported range operation."""


def _step(tag: str, value: Any) -> Any:
    """The +1 step for a discrete range's canonical ``[)`` form."""
    if tag == "daterange":
        return value + _dt.timedelta(days=1)
    return value + 1


def make_range(
    lower: Any, upper: Any, bounds: str, tag: str, *, custom_elem: str | None = None
) -> dict[str, Any]:
    """Build a normalised range subdocument from bounds + a ``[)`` / ``(]`` / ``[]``
    / ``()`` spec. Discrete types canonicalise to ``[)``. An empty range (lower ==
    upper with an exclusive side, or lower > upper) collapses to ``{empty: True}``."""
    if len(bounds) != 2 or bounds[0] not in "[(" or bounds[1] not in ")]":
        raise RangeError(f"invalid range bound flags: {bounds!r}")
    lower_inc = bounds[0] == "["
    upper_inc = bounds[1] == "]"
    _elem, discrete = (custom_elem, False) if custom_elem is not None else RANGE_TYPES[tag]
    # Bounds store in the subtype's canonical form regardless of how they
    # arrived (a ``daterange(date, date)`` constructor bound must match the
    # text-cast path's ISO-text bound).
    from secantus.sql import typemap as _typemap

    if lower is not None:
        lower = _typemap.coerce(lower, _elem)
    if upper is not None:
        upper = _typemap.coerce(upper, _elem)
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
        and (
            _cv(lower) > _cv(upper) or (_cv(lower) == _cv(upper) and not (lower_inc and upper_inc))
        )
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
    value = _cv(value)
    lo, hi = _cv(rng.get("lower")), _cv(rng.get("upper"))
    if lo is not None and (value < lo or (value == lo and not rng.get("lower_inc"))):
        return False
    return not (hi is not None and (value > hi or (value == hi and not rng.get("upper_inc"))))


def _lower_le(a: dict, b: dict) -> bool:
    """Is a's lower bound <= b's lower bound (unbounded lower is smallest)?"""
    la, lb = _cv(a.get("lower")), _cv(b.get("lower"))
    if la is None:
        return True
    if lb is None:
        return False
    if la != lb:
        return la < lb
    return bool(a.get("lower_inc")) or not b.get("lower_inc")


def _upper_ge(a: dict, b: dict) -> bool:
    """Is a's upper bound >= b's upper bound (unbounded upper is largest)?"""
    ua, ub = _cv(a.get("upper")), _cv(b.get("upper"))
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
    up, lo = _cv(hi_side.get("upper")), _cv(lo_side.get("lower"))
    if up is None or lo is None:
        return True
    if up != lo:
        return up > lo
    return bool(hi_side.get("upper_inc")) and bool(lo_side.get("lower_inc"))


def _fmt(value: Any, tag: str | None = None) -> str:
    if value is None:
        return ""
    if isinstance(value, bson.Decimal128):
        return str(value.to_decimal())
    if isinstance(value, _dt.datetime):
        # A ``daterange`` bound is stored as a datetime (BSON has no date-only
        # value) but renders as its date, the way Postgres prints it.
        if tag == "daterange":
            return value.date().isoformat()
        # A stored ``tstzrange`` bound decodes tz-naive UTC from BSON; Postgres
        # renders timestamptz bounds with their UTC offset.
        if tag == "tstzrange" and value.tzinfo is None:
            value = value.replace(tzinfo=_dt.timezone.utc)
        return value.isoformat(sep=" ")
    if isinstance(value, _dt.date):
        return value.isoformat()
    return str(value)


def _quote_bound(text: str) -> str:
    """Double-quote a rendered bound the way Postgres does when it contains
    characters that would confuse the range literal grammar."""
    if text and not any(ch in text for ch in ' ,"\\[](){}'):
        return text
    if text == "":
        return '""'
    # Postgres doubles embedded quotes and backslashes inside a quoted bound.
    return '"' + text.replace("\\", "\\\\").replace('"', '""') + '"'


def render(rng: Any, tag: str | None = None) -> str:
    """Render a range as the Postgres text form ``[lower,upper)`` (``empty`` for the
    empty range). ``tag`` selects element-specific bound rendering."""
    if is_empty(rng) or not isinstance(rng, dict):
        return "empty"
    lb = "[" if rng.get("lower_inc") else "("
    ub = "]" if rng.get("upper_inc") else ")"
    lo, hi = rng.get("lower"), rng.get("upper")
    lo_s = _quote_bound(_fmt(lo, tag)) if lo is not None else ""
    hi_s = _quote_bound(_fmt(hi, tag)) if hi is not None else ""
    return f"{lb}{lo_s},{hi_s}{ub}"


def _pick_lower(a: dict, b: dict, *, smallest: bool) -> dict[str, Any]:
    """Return ``{lower, lower_inc}`` — the smaller (or larger) of the two lower
    bounds. An unbounded lower is the smallest possible."""
    a_le = _lower_le(a, b)
    src = a if (a_le == smallest) else b
    return {"lower": src.get("lower"), "lower_inc": bool(src.get("lower_inc"))}


def _pick_upper(a: dict, b: dict, *, largest: bool) -> dict[str, Any]:
    """Return ``{upper, upper_inc}`` — the larger (or smaller) of the two upper
    bounds. An unbounded upper is the largest possible."""
    a_ge = _upper_ge(a, b)
    src = a if (a_ge == largest) else b
    return {"upper": src.get("upper"), "upper_inc": bool(src.get("upper_inc"))}


def merge(a: Any, b: Any) -> dict[str, Any]:
    """The smallest range covering both ``a`` and ``b`` (``range_merge``). Unlike
    ``union`` this never errors — it spans any gap between disjoint ranges."""
    if is_empty(a):
        return dict(b) if isinstance(b, dict) else {"empty": True}
    if is_empty(b):
        return dict(a) if isinstance(a, dict) else {"empty": True}
    return {**_pick_lower(a, b, smallest=True), **_pick_upper(a, b, largest=True)}


def intersect(a: Any, b: Any) -> dict[str, Any]:
    """The overlap of ``a`` and ``b`` (the ``*`` operator); empty when disjoint."""
    if is_empty(a) or is_empty(b) or not overlaps(a, b):
        return {"empty": True}
    return {**_pick_lower(a, b, smallest=False), **_pick_upper(a, b, largest=False)}


def adjacent(a: Any, b: Any) -> bool:
    """Are ``a`` and ``b`` adjacent (touching with no gap and no overlap)? The
    ``-|-`` operator."""
    if is_empty(a) or is_empty(b) or not isinstance(a, dict) or not isinstance(b, dict):
        return False
    if overlaps(a, b):
        return False
    return _touches(a, b) or _touches(b, a)


def _touches(left: dict, right: dict) -> bool:
    """Does ``left``'s upper bound meet ``right``'s lower bound with exactly one
    side inclusive (so they abut without overlapping or leaving a gap)?"""
    up, lo = _cv(left.get("upper")), _cv(right.get("lower"))
    if up is None or lo is None or up != lo:
        return False
    return bool(left.get("upper_inc")) != bool(right.get("lower_inc"))


def union(a: Any, b: Any) -> dict[str, Any]:
    """The union of ``a`` and ``b`` as a single range (the ``+`` operator). Raises
    when the result would not be contiguous (disjoint and non-adjacent)."""
    if is_empty(a):
        return dict(b) if isinstance(b, dict) else {"empty": True}
    if is_empty(b):
        return dict(a) if isinstance(a, dict) else {"empty": True}
    if not overlaps(a, b) and not adjacent(a, b):
        raise RangeError("result of range union would not be contiguous")
    return {**_pick_lower(a, b, smallest=True), **_pick_upper(a, b, largest=True)}


def difference(a: Any, b: Any) -> dict[str, Any]:
    """``a`` minus ``b`` (the ``-`` operator). Raises when the result would not be a
    single range (``b`` strictly interior to ``a``)."""
    if is_empty(a) or is_empty(b) or not overlaps(a, b):
        return dict(a) if isinstance(a, dict) else {"empty": True}
    left_open = not _lower_le(b, a)  # b starts strictly after a's lower -> left piece
    right_open = not _upper_ge(b, a)  # b ends strictly before a's upper -> right piece
    if left_open and right_open:
        raise RangeError("result of range difference would not be contiguous")
    if left_open:  # keep a's lower up to b's lower
        return {
            "lower": a.get("lower"),
            "lower_inc": bool(a.get("lower_inc")),
            "upper": b.get("lower"),
            "upper_inc": not b.get("lower_inc"),
        }
    if right_open:  # keep b's upper up to a's upper
        return {
            "lower": b.get("upper"),
            "lower_inc": not b.get("upper_inc"),
            "upper": a.get("upper"),
            "upper_inc": bool(a.get("upper_inc")),
        }
    return {"empty": True}  # b covers a


# --------------------------------------------------------------------------- #
# Multiranges: a normalised (sorted, non-overlapping, coalesced) list of ranges,
# stored as ``{"multirange": [range, …]}``.
# --------------------------------------------------------------------------- #

MULTIRANGE_TYPES: dict[str, str] = {
    "int4multirange": "int4range",
    "int8multirange": "int8range",
    "nummultirange": "numrange",
    "tsmultirange": "tsrange",
    "tstzmultirange": "tstzrange",
    "datemultirange": "daterange",
}


# Range type -> the multirange type that aggregates it (``range_agg``).
RANGE_TO_MULTIRANGE: dict[str, str] = {rng: mr for mr, rng in MULTIRANGE_TYPES.items()}


def is_multirange_tag(tag: str | None) -> bool:
    return tag in MULTIRANGE_TYPES


def make_multirange(rngs: list) -> dict[str, Any]:
    """Coalesce a list of ranges into a normalised multirange: drop empties, sort
    by lower bound, and merge overlapping / adjacent members."""
    members = [r for r in rngs if isinstance(r, dict) and not is_empty(r)]
    members.sort(key=_lower_sort_key)
    out: list[dict] = []
    for r in members:
        if out and (overlaps(out[-1], r) or adjacent(out[-1], r)):
            out[-1] = union(out[-1], r)
        else:
            out.append(dict(r))
    return {"multirange": out}


def _lower_sort_key(rng: dict):
    lo = _cv(rng.get("lower"))
    return (0,) if lo is None else (1, lo, 0 if rng.get("lower_inc") else 1)


def multirange_members(mr: Any) -> list:
    return mr.get("multirange", []) if isinstance(mr, dict) else []


def is_multirange(x: Any) -> bool:
    return isinstance(x, dict) and "multirange" in x


def _is_range(x: Any) -> bool:
    return isinstance(x, dict) and ("lower" in x or "empty" in x)


def contains_any(a: Any, b: Any) -> bool:
    """``a @> b`` where either side may be a range, a multirange, or (for ``b``) a
    scalar element. A multirange's members are disjoint and non-adjacent
    (normalised by ``make_multirange``), so a range is contained in a multirange
    iff a *single* member contains it."""
    if is_multirange(a):
        if is_multirange(b):
            return all(_mr_contains_range(a, m) for m in multirange_members(b))
        if _is_range(b):
            return _mr_contains_range(a, b)
        return any(contains_value(m, b) for m in multirange_members(a))  # element
    if is_multirange(b):
        # A single range contains a multirange iff it contains every member (an
        # empty multirange has no members → vacuously contained).
        return all(contains_range(a, m) for m in multirange_members(b))
    if _is_range(b):
        return contains_range(a, b)
    return contains_value(a, b)  # element


def _mr_contains_range(mr: Any, r: Any) -> bool:
    return any(contains_range(m, r) for m in multirange_members(mr))


def overlaps_any(a: Any, b: Any) -> bool:
    """``a && b`` where either side may be a range or a multirange."""
    am = multirange_members(a) if is_multirange(a) else [a]
    bm = multirange_members(b) if is_multirange(b) else [b]
    return any(overlaps(x, y) for x in am for y in bm)


def render_multirange(mr: Any, tag: str | None = None) -> str:
    """Render a multirange as the Postgres text form ``{[1,5),[10,20)}`` (no
    space after the separator — Postgres prints it exactly like this). ``tag``
    is the multirange type; members render as its range type."""
    range_tag = MULTIRANGE_TYPES.get(tag) if tag else None
    return "{" + ",".join(render(r, range_tag) for r in multirange_members(mr)) + "}"


def parse_multirange(
    text: str, tag: str, coerce: Any, *, custom_elem: str | None = None
) -> dict[str, Any]:
    """Parse a multirange text literal ``{[1,5), [10,20)}`` into a normalised
    subdocument. ``tag`` is the multirange type; each member parses as its range."""
    s = text.strip()
    if not (s.startswith("{") and s.endswith("}")):
        raise RangeError(f"malformed multirange literal: {text!r}")
    body = s[1:-1].strip()
    range_tag = MULTIRANGE_TYPES[tag] if custom_elem is None else tag
    if not body:
        return {"multirange": []}
    # Split on commas that sit between a closing bound and the next opening
    # bound — skipping quoted sections so a `","` bound never splits a member.
    parts: list[str] = []
    depth = 0
    start = 0
    i = 0
    while i < len(body):
        ch = body[i]
        if ch == '"':
            i += 1
            while i < len(body):
                if body[i] == "\\":
                    i += 2
                    continue
                if body[i] == '"':
                    break
                i += 1
        elif ch in "[(":
            depth += 1
        elif ch in ")]":
            depth -= 1
        elif ch == "," and depth == 0:
            parts.append(body[start:i])
            start = i + 1
        i += 1
    parts.append(body[start:])
    rngs = [
        parse_literal(p.strip(), range_tag, coerce, custom_elem=custom_elem)
        for p in parts
        if p.strip()
    ]
    return make_multirange(rngs)


# Postgres' range/array literal parser treats only ASCII whitespace as
# trimmable around bounds — Python's ``str.strip()`` also eats NBSP / NEL /
# the \x1c-\x1f separators, which are legitimate bound characters.
_ASCII_WS = " \t\n\r\v\f"


def _parse_bound_token(body: str, i: int) -> tuple[str | None, int]:
    """Parse one bound token starting at ``i``: a double-quoted token (with
    ``""`` and ``\\X`` escapes) or a bare token up to the next comma (with
    ``\\X`` escapes, ASCII-trimmed). Returns ``(value, next_index)`` where a
    missing bare token is None (an infinite bound) but ``""`` is the empty
    string."""
    n = len(body)
    while i < n and body[i] in _ASCII_WS:
        i += 1
    out: list[str] = []
    if i < n and body[i] == '"':
        i += 1
        while i < n:
            c = body[i]
            if c == "\\" and i + 1 < n:
                out.append(body[i + 1])
                i += 2
                continue
            if c == '"':
                if i + 1 < n and body[i + 1] == '"':
                    out.append('"')
                    i += 2
                    continue
                i += 1
                break
            out.append(c)
            i += 1
        while i < n and body[i] in _ASCII_WS:
            i += 1
        return "".join(out), i
    while i < n and body[i] != ",":
        c = body[i]
        if c == "\\" and i + 1 < n:
            out.append(body[i + 1])
            i += 2
            continue
        out.append(c)
        i += 1
    while out and out[-1] in _ASCII_WS:
        out.pop()
    return ("".join(out) or None), i


def parse_literal(
    text: str, tag: str, coerce: Any, *, custom_elem: str | None = None
) -> dict[str, Any]:
    """Parse a range text literal (``[1,10)`` / ``(1,10]`` / ``empty``) into a
    normalised subdocument. ``coerce`` converts a bound token to the element
    type. Bounds follow Postgres' quoting rules: double-quoted tokens keep
    special characters (comma, brackets, whitespace) with ``""``/``\\X``
    escapes; a missing token is an infinite bound."""
    s = text.strip(_ASCII_WS)
    if s.lower() == "empty":
        return {"empty": True}
    if len(s) < 2 or s[0] not in "[(" or s[-1] not in ")]":
        raise RangeError(f"malformed range literal: {text!r}")
    bounds = s[0] + s[-1]
    body = s[1:-1]
    lo_s, i = _parse_bound_token(body, 0)
    if i >= len(body) or body[i] != ",":
        raise RangeError(f"malformed range literal: {text!r}")
    hi_s, i = _parse_bound_token(body, i + 1)
    if i < len(body):
        raise RangeError(f"malformed range literal: {text!r}")
    lo = coerce(lo_s) if lo_s is not None else None
    hi = coerce(hi_s) if hi_s is not None else None
    return make_range(lo, hi, bounds, tag, custom_elem=custom_elem)


def _canonical_bound(v: Any) -> Any:
    """A comparison-stable form of a range bound: ``Decimal128`` unwraps to
    ``Decimal`` (so int / Decimal / Decimal128 spellings of the same number
    compare equal), naive datetimes read as UTC, date objects as ISO text."""
    if isinstance(v, bson.Decimal128):
        return v.to_decimal()
    if isinstance(v, _dt.datetime):
        return v.replace(tzinfo=_dt.timezone.utc) if v.tzinfo is None else v
    if isinstance(v, _dt.date):
        return v.isoformat()
    return v


def canonical(rng: Any) -> tuple:
    """A hashable, representation-independent identity for a range subdocument —
    what equality compares (a ``numrange(…)`` constructor's int bound must equal
    the text cast's ``Decimal128``)."""
    r = rng if isinstance(rng, dict) else {}
    if r.get("empty"):
        return ("empty",)
    return (
        _canonical_bound(r.get("lower")),
        _canonical_bound(r.get("upper")),
        bool(r.get("lower_inc")),
        bool(r.get("upper_inc")),
    )


def canonical_multirange(mr: Any) -> tuple:
    return tuple(canonical(r) for r in mr.get("multirange", []))
