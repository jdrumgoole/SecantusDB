"""Postgres geometric types: ``point`` / ``box`` / ``circle`` / ``polygon`` /
``lseg`` (and the ``line`` / ``path`` spellings, stored but not operated on).

Values are stored as their canonical Postgres text (``point`` ``(1,2)``, ``box``
``(2,2),(0,0)``, ``circle`` ``<(0,0),5>``, ``polygon`` ``((0,0),(1,0),(1,1))``,
``lseg`` ``[(0,0),(1,1)]``) — BSON-safe, and the canonical form is
self-describing so the ``@>`` / ``<@`` / ``&&`` / ``<->`` operators can
auto-detect the geometry from the text. Geometry math delegates to Shapely (the
same library ``secantus.geo`` uses); a ``circle`` is modelled as its centre point
buffered by the radius.

Out of scope: the infinite ``line`` type and ``path`` open/closed distinction
for the operators, ``#`` (intersection point) / ``##`` (closest point) / ``?-`` /
``?|`` positional operators, and geometric indexes.
"""

from __future__ import annotations

import re
from typing import Any

from shapely.geometry import LineString, Point, Polygon

_GEO_TAGS = frozenset({"point", "box", "circle", "polygon", "lseg", "line", "path"})

_NUM = r"[-+]?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?"
_PAIR_RE = re.compile(rf"\(\s*({_NUM})\s*,\s*({_NUM})\s*\)")
_CIRCLE_RE = re.compile(rf"^<\s*\(\s*({_NUM})\s*,\s*({_NUM})\s*\)\s*,\s*({_NUM})\s*>$")


class GeoError(ValueError):
    """A malformed geometric literal."""


def _num(s: str) -> float:
    f = float(s)
    return int(f) if f == int(f) else f


def _pairs(text: str) -> list[tuple[float, float]]:
    out = [(_num(a), _num(b)) for a, b in _PAIR_RE.findall(text)]
    if not out:
        raise GeoError(f"no coordinate pairs in geometry: {text!r}")
    return out


def _fmt_pair(p: tuple[float, float]) -> str:
    return f"({_fmt(p[0])},{_fmt(p[1])})"


def _fmt(n: float) -> str:
    return str(int(n)) if float(n) == int(n) else str(n)


def canonical(value: Any, tag: str) -> str:
    """Normalise a geometric literal to its canonical Postgres text for ``tag``."""
    s = str(value).strip()
    if tag == "circle":
        m = _CIRCLE_RE.match(s)
        if m:
            cx, cy, r = _num(m.group(1)), _num(m.group(2)), _num(m.group(3))
        else:
            pts = _pairs(s)
            nums = re.findall(_NUM, s)
            r = _num(nums[-1]) if nums else 0
            cx, cy = pts[0]
        return f"<{_fmt_pair((cx, cy))},{_fmt(r)}>"
    if tag == "line":
        # ``line``'s canonical text is ``{A,B,C}`` — three coefficients, no
        # coordinate pairs — so it has to be handled before ``_pairs``. The
        # two-point spelling ``[(x1,y1),(x2,y2)]`` is also accepted on input and
        # converted, the way Postgres does.
        if s.startswith("{"):
            nums = re.findall(_NUM, s)
            if len(nums) < 3:
                raise GeoError(f"invalid line: {value!r}")
            return "{" + ",".join(_fmt(_num(n)) for n in nums[:3]) + "}"
        (x1, y1), (x2, y2) = _pairs(s)[:2]
        return line_from_points(x1, y1, x2, y2)
    pts = _pairs(s)
    if tag == "point":
        return _fmt_pair(pts[0])
    if tag == "box":
        (x1, y1), (x2, y2) = pts[0], pts[1]
        hi = (max(x1, x2), max(y1, y2))
        lo = (min(x1, x2), min(y1, y2))
        return f"{_fmt_pair(hi)},{_fmt_pair(lo)}"
    if tag == "lseg":
        return f"[{_fmt_pair(pts[0])},{_fmt_pair(pts[1])}]"
    if tag == "path":
        # An open path keeps the ``[…]`` spelling; a closed one uses ``(…)``.
        # The distinction is part of the value, not just presentation, so it has
        # to survive the round trip even though the operators ignore it.
        body = ",".join(_fmt_pair(p) for p in pts)
        return f"[{body}]" if s.startswith("[") else f"({body})"
    if tag == "polygon":
        return "(" + ",".join(_fmt_pair(p) for p in pts) + ")"
    return s


def line_from_points(x1: float, y1: float, x2: float, y2: float) -> str:
    """The ``{A,B,C}`` text for the infinite line through two points, following
    ``line_construct_pts`` in Postgres (vertical and horizontal are special-cased
    so the coefficients come out exact rather than as a division artefact)."""
    if x1 == x2:
        if y1 == y2:
            raise GeoError("invalid line specification: must be two distinct points")
        a, b, c = -1.0, 0.0, x1
    elif y1 == y2:
        a, b, c = 0.0, -1.0, y1
    else:
        a = (y2 - y1) / (x2 - x1)
        b = -1.0
        c = y1 - a * x1
    return "{" + ",".join(_fmt(v) for v in (a, b, c)) + "}"


def is_geo_text(v: Any) -> bool:
    """Whether ``v`` is a string that looks like a stored geometry (so the shared
    ``@>`` / ``&&`` operators can tell a geo operand from other text)."""
    if not isinstance(v, str):
        return False
    s = v.strip()
    return bool(_PAIR_RE.search(s)) and (s[0] in "(<[")


def to_shapely(value: Any):
    """Convert a canonical geometry text to a Shapely geometry (a ``circle`` becomes
    its centre buffered by the radius). The form is auto-detected from the text."""
    s = str(value).strip()
    if s.startswith("{"):  # line — infinite, so it has no Shapely counterpart
        raise GeoError(f"geometric operators are not supported on line: {value!r}")
    if s.startswith("<"):  # circle
        m = _CIRCLE_RE.match(s)
        if m is None:
            raise GeoError(f"invalid circle: {value!r}")
        cx, cy, r = _num(m.group(1)), _num(m.group(2)), _num(m.group(3))
        return Point(cx, cy).buffer(abs(r))
    pts = _pairs(s)
    if len(pts) == 1:  # point
        return Point(pts[0])
    if s.startswith("["):  # lseg / open path
        return LineString(pts)
    if s.startswith("(("):  # polygon / closed path
        return Polygon(pts)
    if len(pts) == 2:  # box: two opposite corners
        (x1, y1), (x2, y2) = pts
        return Polygon([(x1, y1), (x2, y1), (x2, y2), (x1, y2)])
    return Polygon(pts)


def distance(a: Any, b: Any) -> float:
    """``a <-> b`` — the shortest distance between two geometries."""
    d = to_shapely(a).distance(to_shapely(b))
    return int(d) if float(d) == int(d) else round(d, 6)


def contains(a: Any, b: Any) -> bool:
    """``a @> b`` — does ``a`` contain ``b``?"""
    return to_shapely(a).covers(to_shapely(b))


def overlaps(a: Any, b: Any) -> bool:
    """``a && b`` — do the two geometries intersect (bounding-box overlap in
    Postgres, but geometric intersection is the more useful test here)?"""
    return to_shapely(a).intersects(to_shapely(b))
