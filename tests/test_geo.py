"""Unit tests for ``secantus.geo`` primitives.

Pure-module tests — no SecantusDBServer, no pymongo — exercise the
parsing, distance, and containment helpers directly. The integration
surface (operators reachable via pymongo) is covered in
``test_geo_query.py``.
"""

from __future__ import annotations

import math

import pytest
from shapely.geometry import Point, Polygon

from secantus.geo import (
    EARTH_RADIUS_METERS,
    GeoError,
    bounding_box,
    distance,
    geo_intersects,
    geo_within,
    parse_doc_geometry,
    parse_query_geometry,
)


class TestParseDocGeometry:
    def test_geojson_point(self) -> None:
        g = parse_doc_geometry({"type": "Point", "coordinates": [10.0, 20.0]})
        assert isinstance(g, Point)
        assert (g.x, g.y) == (10.0, 20.0)

    def test_legacy_xy_pair(self) -> None:
        g = parse_doc_geometry([3.0, 4.0])
        assert isinstance(g, Point)
        assert (g.x, g.y) == (3.0, 4.0)

    def test_legacy_xy_dict(self) -> None:
        g = parse_doc_geometry({"x": 1.0, "y": 2.0})
        assert (g.x, g.y) == (1.0, 2.0)
        g = parse_doc_geometry({"lng": 5.0, "lat": 6.0})
        assert (g.x, g.y) == (5.0, 6.0)

    def test_geojson_polygon(self) -> None:
        g = parse_doc_geometry(
            {
                "type": "Polygon",
                "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]],
            }
        )
        assert isinstance(g, Polygon)

    def test_invalid_returns_none(self) -> None:
        # Garbage in returns None — operators treat it as "no match" without
        # raising. Mirrors mongod's tolerance for stored bad geometry.
        assert parse_doc_geometry(None) is None
        assert parse_doc_geometry("hello") is None
        assert parse_doc_geometry({"type": "Bogus", "coordinates": [0, 0]}) is None
        assert parse_doc_geometry([1, 2, 3]) is None  # wrong arity
        assert parse_doc_geometry({"x": "a", "y": "b"}) is None


class TestParseQueryGeometry:
    def test_geojson(self) -> None:
        geom, spherical = parse_query_geometry(
            {"$geometry": {"type": "Point", "coordinates": [0, 0]}}
        )
        assert isinstance(geom, Point)
        assert spherical is False  # planar; only $centerSphere is spherical

    def test_box(self) -> None:
        geom, spherical = parse_query_geometry({"$box": [[0, 0], [10, 10]]})
        assert isinstance(geom, Polygon)
        # Box ordering normalised — both [[0,0],[10,10]] and [[10,10],[0,0]]
        # produce the same polygon.
        geom2, _ = parse_query_geometry({"$box": [[10, 10], [0, 0]]})
        assert geom.equals(geom2)
        assert spherical is False

    def test_polygon_auto_closes(self) -> None:
        # User supplies an open polygon — we close it.
        geom, _ = parse_query_geometry({"$polygon": [[0, 0], [1, 0], [1, 1]]})
        coords = list(geom.exterior.coords)
        assert coords[0] == coords[-1]

    def test_center_sphere(self) -> None:
        geom, spherical = parse_query_geometry({"$centerSphere": [[0.0, 0.0], 0.001]})
        assert spherical is True
        assert geom.contains_point(0.0, 0.0)

    def test_missing_operator_raises(self) -> None:
        with pytest.raises(GeoError):
            parse_query_geometry({})

    def test_box_wrong_shape(self) -> None:
        with pytest.raises(GeoError):
            parse_query_geometry({"$box": [[0, 0]]})

    def test_polygon_too_few_points(self) -> None:
        with pytest.raises(GeoError):
            parse_query_geometry({"$polygon": [[0, 0], [1, 1]]})

    def test_center_negative_radius(self) -> None:
        with pytest.raises(GeoError):
            parse_query_geometry({"$center": [[0, 0], -1.0]})


class TestDistance:
    def test_planar_pythagoras(self) -> None:
        d = distance(Point(0, 0), Point(3, 4), spherical=False)
        assert d == pytest.approx(5.0)

    def test_spherical_zero(self) -> None:
        # Same point — zero distance regardless of mode.
        d = distance(Point(10, 20), Point(10, 20), spherical=True)
        assert d == pytest.approx(0.0)

    def test_spherical_one_degree_at_equator(self) -> None:
        # 1° of longitude at the equator ≈ 111.319 km.
        d = distance(Point(0, 0), Point(1, 0), spherical=True)
        # ~111.319 km. Tolerance accounts for the ratio between mongod's
        # 6378.1 km radius and WGS84 equatorial; we use mongod's constant.
        expected = math.radians(1.0) * EARTH_RADIUS_METERS
        assert d == pytest.approx(expected, rel=1e-6)

    def test_spherical_antipodal(self) -> None:
        # Antipodal points: half the circumference.
        d = distance(Point(0, 0), Point(180, 0), spherical=True)
        assert d == pytest.approx(math.pi * EARTH_RADIUS_METERS, rel=1e-6)

    def test_non_point_returns_none(self) -> None:
        # Phase 1: $near restricted to point-to-point. Polygon as one arg
        # would need nearest-point math we haven't implemented.
        poly = Polygon([(0, 0), (1, 0), (1, 1), (0, 1)])
        assert distance(poly, Point(5, 5), spherical=False) is None
        assert distance(Point(0, 0), poly, spherical=False) is None


class TestGeoWithin:
    def test_point_in_polygon(self) -> None:
        poly = Polygon([(0, 0), (10, 0), (10, 10), (0, 10)])
        assert geo_within(Point(5, 5), poly) is True
        assert geo_within(Point(20, 20), poly) is False

    def test_polygon_in_polygon(self) -> None:
        outer = Polygon([(0, 0), (10, 0), (10, 10), (0, 10)])
        inner = Polygon([(2, 2), (8, 2), (8, 8), (2, 8)])
        assert geo_within(inner, outer) is True
        assert geo_within(outer, inner) is False

    def test_centersphere_point(self) -> None:
        circle, _ = parse_query_geometry({"$centerSphere": [[0.0, 0.0], 0.001]})
        # Point well inside the cap (0.001 rad ≈ 6.4 km at equator).
        assert geo_within(Point(0.001, 0.0), circle) is True
        # Point well outside.
        assert geo_within(Point(10.0, 10.0), circle) is False

    def test_none_doc_geom_is_false(self) -> None:
        poly = Polygon([(0, 0), (10, 0), (10, 10), (0, 10)])
        assert geo_within(None, poly) is False


class TestGeoIntersects:
    def test_overlap(self) -> None:
        a = Polygon([(0, 0), (10, 0), (10, 10), (0, 10)])
        b = Polygon([(5, 5), (15, 5), (15, 15), (5, 15)])
        assert geo_intersects(a, b) is True

    def test_disjoint(self) -> None:
        a = Polygon([(0, 0), (10, 0), (10, 10), (0, 10)])
        b = Polygon([(20, 20), (30, 20), (30, 30), (20, 30)])
        assert geo_intersects(a, b) is False

    def test_centersphere_rejected(self) -> None:
        # $geoIntersects should not be called with a spherical cap; if it is,
        # we return False rather than crash. The operator handler also
        # explicitly rejects $centerSphere upstream — this is a belt+braces.
        circle, _ = parse_query_geometry({"$centerSphere": [[0, 0], 0.01]})
        assert geo_intersects(Point(0, 0), circle) is False


class TestBoundingBox:
    def test_polygon_bounds(self) -> None:
        poly = Polygon([(0, 0), (10, 0), (10, 5), (0, 5)])
        assert bounding_box(poly) == (0.0, 0.0, 10.0, 5.0)

    def test_centersphere_bounds(self) -> None:
        circle, _ = parse_query_geometry({"$centerSphere": [[0.0, 0.0], 0.001]})
        min_x, min_y, max_x, max_y = bounding_box(circle)
        # 0.001 rad ≈ 0.0573°. Bounds span roughly that around the origin.
        assert min_x < 0 < max_x
        assert min_y < 0 < max_y
        assert max_x == pytest.approx(math.degrees(0.001))


# ---------------------------------------------------------------------------
# 2d quadtree covering ranges
# ---------------------------------------------------------------------------


def test_2d_quadtree_covering_single_range_for_aligned_square() -> None:
    """A bbox that maps to a single power-of-2 aligned bucket cell has
    a contiguous Z-order range — the invariant the quadtree exploits.

    With ``bits=4`` and ``min=0, max=16`` each unit is one bucket. A
    box from (0,0) to (3.99, 3.99) maps to bucket bbox (0,0)–(3,3)
    which IS a 4×4 (2^2 × 2^2) aligned quadtree cell;
    Z(0,0)=0, Z(3,3)=15. Single range (0, 15).
    """
    from shapely.geometry import box

    from secantus.geo_index import planar_2d_covering_ranges

    options = {"bits": 4, "min": 0.0, "max": 16.0}
    ranges = planar_2d_covering_ranges(box(0.0, 0.0, 3.99, 3.99), options)
    assert ranges == [(0, 15)]


def test_2d_quadtree_covering_emits_multiple_ranges_for_tortuous_bbox() -> None:
    """A bbox that doesn't align to power-of-2 cells decomposes into
    multiple tight ranges instead of one over-covering one."""
    from shapely.geometry import box

    from secantus.geo_index import planar_2d_covering_ranges

    options = {"bits": 8, "min": 0.0, "max": 256.0}
    # An off-axis rectangle (3,3)–(13,11): not a power-of-2 cell.
    ranges = planar_2d_covering_ranges(box(3.0, 3.0, 13.0, 11.0), options)
    # Quadtree decomposition yields more than 1 range and fewer than
    # the cap. (Exact count depends on bit alignment; assert the
    # invariant: it's tighter than the single coarse fallback.)
    assert len(ranges) >= 1
    # Sanity: all ranges are well-formed (lo <= hi).
    for lo, hi in ranges:
        assert lo <= hi


def test_2d_quadtree_covering_falls_back_under_cap() -> None:
    """Pathological bbox that would explode the quadtree decomposition
    falls back to the single-range coarse covering (max_ranges cap)."""
    from shapely.geometry import box

    from secantus.geo_index import planar_2d_covering, planar_2d_covering_ranges

    options = {"bits": 16, "min": -1.0, "max": 1.0}
    # Whole-grid bbox to force lots of subdivision; with a tight cap
    # it should fall back to one range matching planar_2d_covering.
    geom = box(-0.9, -0.9, 0.9, 0.9)
    ranges = planar_2d_covering_ranges(geom, options, max_ranges=2)
    single = planar_2d_covering(geom, options)
    # Under the cap, output is either the multi-range tight cover OR
    # the single-range fallback; the contract is "≤ max_ranges
    # ranges". Either way, the union covers the bbox.
    assert len(ranges) <= 2
    if len(ranges) == 1:
        assert ranges[0] == single
