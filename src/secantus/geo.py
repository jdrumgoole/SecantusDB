"""Geospatial primitives for ``2d`` / ``2dsphere`` query operators.

Pure module (no I/O, no Storage import). Owns three responsibilities:

1. **Parse** doc field values and query geometries — coerces what the wire
   delivers (GeoJSON dicts, legacy ``[x, y]`` pairs, ``{x, y}`` maps, etc.)
   into a uniform internal form.
2. **Test** containment / intersection — `geo_within`, `geo_intersects`.
   Planar tests delegate to Shapely; spherical-circle tests
   (``$centerSphere``) are implemented directly because Shapely is planar.
3. **Measure** distance — `distance` returns meters for spherical mode and
   the input's planar units for planar mode, mirroring ``mongod``.

The module deliberately accepts more shapes than MongoDB's strict spec:
real-world clients send GeoJSON with mixed casing, missing ``type`` fields,
nested coordinates, etc. We err on the side of "best-effort coercion"
inside :func:`parse_doc_geometry` (which never raises — bad geometry
silently does not match any query) but enforce strict shapes inside
:func:`parse_query_geometry` (which raises on malformed input — the
client's query is wrong and they need a clear error).

Distance constants match ``mongod`` defaults: ``$centerSphere`` /
``$nearSphere`` use mean Earth radius **6,378,100 m** (per MongoDB's
documented constant), not 6,378,137 m (WGS84 equatorial). Tests assert
this exact value so a future "fix" doesn't silently break parity.
"""

from __future__ import annotations

import math
from collections.abc import Mapping, Sequence
from typing import Any

from shapely import wkt as _shapely_wkt  # noqa: F401 — keeps shapely import warm
from shapely.errors import GEOSException
from shapely.geometry import (
    LineString,
    MultiLineString,
    MultiPoint,
    MultiPolygon,
    Point,
    Polygon,
)
from shapely.geometry.base import BaseGeometry

# Mean Earth radius in meters, matching mongod's `$centerSphere` /
# `$nearSphere` constant. Documented at
# https://www.mongodb.com/docs/manual/reference/operator/query/centerSphere/.
EARTH_RADIUS_METERS: float = 6_378_100.0


class GeoError(ValueError):
    """Malformed query geometry. Surfaced as ``BadValue`` to the client."""


# ---------------------------------------------------------------------------
# Doc-side parsing
# ---------------------------------------------------------------------------


def parse_doc_geometry(value: Any) -> BaseGeometry | None:
    """Coerce a doc field value into a Shapely geometry.

    Accepts:
      - GeoJSON dicts: ``{"type": "Point", "coordinates": [lng, lat]}``,
        Polygon, LineString, MultiPoint, MultiLineString, MultiPolygon.
      - Legacy ``[x, y]`` pairs (interpreted as a planar Point).
      - ``{"x": ..., "y": ...}`` / ``{"lng": ..., "lat": ...}`` maps
        (interpreted as a planar Point).

    Returns ``None`` for any value that cannot be coerced — geo operators
    treat ``None`` as "did not match" rather than raising, mirroring
    ``mongod``: a doc with bad geometry simply doesn't appear in results.
    """
    if value is None:
        return None
    if isinstance(value, BaseGeometry):
        return value
    if isinstance(value, Mapping):
        # GeoJSON shape first.
        gtype = value.get("type")
        if isinstance(gtype, str):
            return _from_geojson(value)
        # Legacy {x, y} or {lng, lat} pair.
        for keys in (("x", "y"), ("lng", "lat"), ("longitude", "latitude")):
            if keys[0] in value and keys[1] in value:
                try:
                    return Point(float(value[keys[0]]), float(value[keys[1]]))
                except (TypeError, ValueError):
                    return None
        return None
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes)) and len(value) == 2:
        try:
            return Point(float(value[0]), float(value[1]))
        except (TypeError, ValueError):
            return None
    return None


_GEOJSON_BUILDERS: dict[str, Any] = {
    "Point": lambda c: Point(*c),
    "LineString": LineString,
    "Polygon": lambda c: Polygon(c[0], holes=c[1:] if len(c) > 1 else None),
    "MultiPoint": MultiPoint,
    "MultiLineString": MultiLineString,
    "MultiPolygon": lambda c: MultiPolygon(
        [Polygon(p[0], holes=p[1:] if len(p) > 1 else None) for p in c]
    ),
}


def _from_geojson(value: Mapping[str, Any]) -> BaseGeometry | None:
    gtype = value.get("type")
    coords = value.get("coordinates")
    builder = _GEOJSON_BUILDERS.get(gtype)
    if builder is None or coords is None:
        return None
    try:
        return builder(coords)
    except (TypeError, ValueError, GEOSException):
        return None


# ---------------------------------------------------------------------------
# Query-side parsing
# ---------------------------------------------------------------------------


def parse_query_geometry(
    spec: Mapping[str, Any],
) -> tuple[BaseGeometry | _SphericalCircle, bool]:
    """Build a query geometry from ``$geoWithin`` / ``$geoIntersects`` arg.

    Returns ``(geometry, is_spherical)``. ``is_spherical`` is True only for
    ``$centerSphere``; everything else is planar (Shapely's native model).
    Even ``$geometry`` with GeoJSON returns planar — Shapely handles
    GeoJSON polygons treating coordinates as a flat plane, which matches
    ``mongod``'s "approximate" behaviour for small areas. Spherical tests
    are reserved for explicit ``$centerSphere`` / ``$nearSphere``.
    """
    if "$geometry" in spec:
        geo = _from_geojson(spec["$geometry"])
        if geo is None:
            raise GeoError("$geometry must be valid GeoJSON")
        return geo, False
    if "$box" in spec:
        return _parse_box(spec["$box"]), False
    if "$polygon" in spec:
        return _parse_polygon(spec["$polygon"]), False
    if "$center" in spec:
        return _parse_center(spec["$center"]), False
    if "$centerSphere" in spec:
        return _parse_center_sphere(spec["$centerSphere"]), True
    raise GeoError("geo query requires $geometry, $box, $polygon, $center, or $centerSphere")


def _parse_box(box: Any) -> Polygon:
    if not (isinstance(box, Sequence) and len(box) == 2):
        raise GeoError("$box must be a 2-element list of corner points")
    (x1, y1), (x2, y2) = (_pair(p, "$box corner") for p in box)
    lo_x, hi_x = sorted((x1, x2))
    lo_y, hi_y = sorted((y1, y2))
    return Polygon([(lo_x, lo_y), (hi_x, lo_y), (hi_x, hi_y), (lo_x, hi_y), (lo_x, lo_y)])


def _parse_polygon(polygon: Any) -> Polygon:
    if not (isinstance(polygon, Sequence) and len(polygon) >= 3):
        raise GeoError("$polygon must have at least 3 points")
    pts = [_pair(p, "$polygon point") for p in polygon]
    if pts[0] != pts[-1]:
        pts.append(pts[0])
    return Polygon(pts)


def _parse_center(center: Any) -> Polygon:
    if not (isinstance(center, Sequence) and len(center) == 2):
        raise GeoError("$center must be [[x, y], radius]")
    cx, cy = _pair(center[0], "$center origin")
    r = _number(center[1], "$center radius")
    if r < 0:
        raise GeoError("$center radius must be non-negative")
    # Approximate the disk with a 64-vertex polygon. Inside Shapely's
    # `within`/`intersects` this is the same approximation `mongod` makes
    # internally (server uses a circle approximation too — the user-visible
    # behavior is consistent across drivers and our surrogate).
    return Point(cx, cy).buffer(r, quad_segs=16)


def _parse_center_sphere(center_sphere: Any) -> _SphericalCircle:
    if not (isinstance(center_sphere, Sequence) and len(center_sphere) == 2):
        raise GeoError("$centerSphere must be [[lng, lat], radius_radians]")
    lng, lat = _pair(center_sphere[0], "$centerSphere origin")
    r_rad = _number(center_sphere[1], "$centerSphere radius")
    if r_rad < 0:
        raise GeoError("$centerSphere radius must be non-negative")
    return _SphericalCircle(lng, lat, r_rad)


def _pair(value: Any, label: str) -> tuple[float, float]:
    if not (isinstance(value, Sequence) and len(value) == 2):
        raise GeoError(f"{label} must be a [x, y] pair")
    return _number(value[0], label), _number(value[1], label)


def _number(value: Any, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise GeoError(f"{label} must be a number")
    return float(value)


# ---------------------------------------------------------------------------
# Spherical-circle (used for $centerSphere / $nearSphere only)
# ---------------------------------------------------------------------------


class _SphericalCircle:
    """A circle on a unit sphere, defined by ``[lng, lat]`` + radius in radians.

    Deliberately **not** a Shapely subclass — Shapely 2.x's
    ``BaseGeometry.__new__`` rejects extra args, and we don't need any
    Shapely operations on this object anyway. The dispatch in
    :func:`geo_within` / :func:`geo_intersects` does
    ``isinstance(query_geom, _SphericalCircle)`` and routes to the
    great-circle path before Shapely is consulted.
    """

    __slots__ = ("center_lng", "center_lat", "radius_rad")

    def __init__(self, lng: float, lat: float, radius_rad: float) -> None:
        self.center_lng = lng
        self.center_lat = lat
        self.radius_rad = radius_rad

    def contains_point(self, lng: float, lat: float) -> bool:
        return _great_circle_radians(self.center_lng, self.center_lat, lng, lat) <= self.radius_rad


# ---------------------------------------------------------------------------
# Containment / intersection
# ---------------------------------------------------------------------------


def geo_within(
    doc_geom: BaseGeometry | None,
    query_geom: BaseGeometry | _SphericalCircle,
) -> bool:
    """True iff `doc_geom` is fully contained by `query_geom`.

    For Shapely shapes, uses planar `within`. For ``_SphericalCircle``,
    every point of doc_geom must be inside the great-circle disk — for a
    Point doc this is one test; for non-Points we approximate by testing
    the bounding-box vertices, matching ``mongod``'s coarse behavior.
    """
    if doc_geom is None:
        return False
    if isinstance(query_geom, _SphericalCircle):
        if isinstance(doc_geom, Point):
            return query_geom.contains_point(doc_geom.x, doc_geom.y)
        # For non-point doc geometry, require every vertex inside the cap.
        return all(query_geom.contains_point(x, y) for x, y in _iter_coords(doc_geom))
    try:
        return bool(doc_geom.within(query_geom)) or bool(doc_geom.equals(query_geom))
    except GEOSException:
        return False


def geo_intersects(
    doc_geom: BaseGeometry | None,
    query_geom: BaseGeometry | _SphericalCircle,
) -> bool:
    """True iff `doc_geom` and `query_geom` share at least one point.

    ``$geoIntersects`` is undefined for ``$centerSphere`` / spherical caps
    in real ``mongod``; we follow suit and reject it at the operator
    handler. This function only sees Shapely-side geometries.
    """
    if doc_geom is None or isinstance(query_geom, _SphericalCircle):
        return False
    try:
        return bool(doc_geom.intersects(query_geom))
    except GEOSException:
        return False


def _iter_coords(geom: BaseGeometry) -> list[tuple[float, float]]:
    """All (x, y) coordinates in `geom`. Polygons return exterior ring;
    multi-* shapes flatten."""
    coords: list[tuple[float, float]] = []
    if isinstance(geom, Point):
        coords.append((geom.x, geom.y))
    elif isinstance(geom, (LineString, MultiPoint)):
        coords.extend((x, y) for x, y in geom.coords)
    elif isinstance(geom, Polygon):
        coords.extend((x, y) for x, y in geom.exterior.coords)
    elif isinstance(geom, MultiLineString):
        for ls in geom.geoms:
            coords.extend((x, y) for x, y in ls.coords)
    elif isinstance(geom, MultiPolygon):
        for poly in geom.geoms:
            coords.extend((x, y) for x, y in poly.exterior.coords)
    return coords


# ---------------------------------------------------------------------------
# Coordinate-bounds validation (used by index extraction)
# ---------------------------------------------------------------------------


# 2dsphere bounds are baked in (the unit sphere; mongod does the same).
# 2d bounds are user-tunable via the index's `min` / `max` options.
_2DSPHERE_LNG_BOUNDS: tuple[float, float] = (-180.0, 180.0)
_2DSPHERE_LAT_BOUNDS: tuple[float, float] = (-90.0, 90.0)


def validate_coordinates(
    geom: BaseGeometry,
    *,
    geo_type: str,
    options: Mapping[str, Any] | None = None,
) -> None:
    """Walk every coordinate in ``geom`` and raise :class:`GeoError` if
    any falls outside the bounds dictated by ``geo_type``.

    For ``2dsphere`` geometries we enforce the unit-sphere range
    (``lng ∈ [-180, 180]``, ``lat ∈ [-90, 90]``) regardless of options.

    For ``2d`` geometries we enforce the user-configurable
    ``min`` / ``max`` (defaulting to ``[-180, 180]``) on both axes.

    Index-time callers in :mod:`secantus.storage` raise the resulting
    error to surface it as a wire-level "can't extract geo keys" write
    error (code 16572), matching ``mongod``.
    """
    if geo_type == "2dsphere":
        for x, y in _iter_coords(geom):
            if not (_2DSPHERE_LNG_BOUNDS[0] <= x <= _2DSPHERE_LNG_BOUNDS[1]):
                raise GeoError(f"longitude {x} out of 2dsphere range [-180, 180]")
            if not (_2DSPHERE_LAT_BOUNDS[0] <= y <= _2DSPHERE_LAT_BOUNDS[1]):
                raise GeoError(f"latitude {y} out of 2dsphere range [-90, 90]")
        return
    if geo_type == "2d":
        opts = options or {}
        lo = float(opts.get("min", -180.0))
        hi = float(opts.get("max", 180.0))
        for x, y in _iter_coords(geom):
            if not (lo <= x <= hi):
                raise GeoError(f"x coordinate {x} out of 2d range [{lo}, {hi}]")
            if not (lo <= y <= hi):
                raise GeoError(f"y coordinate {y} out of 2d range [{lo}, {hi}]")
        return
    raise GeoError(f"unknown geo index type: {geo_type!r}")


# ---------------------------------------------------------------------------
# Distance
# ---------------------------------------------------------------------------


def distance(a: BaseGeometry, b: BaseGeometry, *, spherical: bool) -> float | None:
    """Distance between two point-like geometries.

    Spherical mode returns **meters** (haversine on a sphere of radius
    :data:`EARTH_RADIUS_METERS`). Planar mode returns the input's units.
    Returns ``None`` if either input cannot be reduced to a point — for
    non-point ``$near``-style queries, ``mongod`` measures from the
    nearest point on the geometry, but for the surrogate we restrict
    ``$near`` to point queries (which is the documented common case).
    """
    pa = _as_point(a)
    pb = _as_point(b)
    if pa is None or pb is None:
        return None
    if spherical:
        return _great_circle_radians(pa[0], pa[1], pb[0], pb[1]) * EARTH_RADIUS_METERS
    return math.hypot(pa[0] - pb[0], pa[1] - pb[1])


def _as_point(geom: BaseGeometry) -> tuple[float, float] | None:
    if isinstance(geom, Point):
        return (geom.x, geom.y)
    return None


def _great_circle_radians(lng_a: float, lat_a: float, lng_b: float, lat_b: float) -> float:
    """Haversine angle between two ``[lng, lat]`` points, in radians."""
    phi_a = math.radians(lat_a)
    phi_b = math.radians(lat_b)
    d_phi = phi_b - phi_a
    d_lambda = math.radians(lng_b - lng_a)
    h = (
        math.sin(d_phi / 2.0) ** 2
        + math.cos(phi_a) * math.cos(phi_b) * math.sin(d_lambda / 2.0) ** 2
    )
    # Numerical clamp — `h` can drift slightly above 1.0 for antipodal
    # points due to float rounding, which would NaN the asin.
    h = min(max(h, 0.0), 1.0)
    return 2.0 * math.asin(math.sqrt(h))


# ---------------------------------------------------------------------------
# Bounding box (used by Phase 2's index picker)
# ---------------------------------------------------------------------------


def bounding_box(
    geom: BaseGeometry | _SphericalCircle,
) -> tuple[float, float, float, float]:
    """Return ``(min_x, min_y, max_x, max_y)`` for `geom`."""
    if isinstance(geom, _SphericalCircle):
        # Spherical cap bounds in lng/lat — useful as a coarse pre-filter.
        # This is a coarse box (latitude bounds shrink with the cap), but
        # the index picker is just a candidate-narrowing step.
        r_deg = math.degrees(geom.radius_rad)
        return (
            geom.center_lng - r_deg,
            geom.center_lat - r_deg,
            geom.center_lng + r_deg,
            geom.center_lat + r_deg,
        )
    return tuple(geom.bounds)  # type: ignore[return-value]
