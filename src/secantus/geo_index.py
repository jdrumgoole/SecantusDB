"""Cell-ID encoding for ``2dsphere`` and ``2d`` indexes.

Sits between ``secantus.geo`` (geometry primitives) and
``secantus.storage`` (WiredTiger-backed entry table). Owns three things:

1. Compute a *covering set* of cells for a doc's geometry — the list of
   cells whose union contains the geometry. Each cell becomes one entry
   in the index, so a polygon spanning 12 S2 cells writes 12 entries.
2. Compute a covering set for a *query* geometry — the picker uses this
   to range-scan the entries table for candidate ``_id`` values.
3. Encode a cell ID as fixed-width 8 bytes — the entries-table key is
   ``(db, coll, name, encode_cell(cell) + COMPOUND_SEP + id_key)``,
   and fixed-width keeps lex byte ordering aligned with numeric cell-ID
   ordering so a `WT` range scan visits cells in S2 / geohash order.

The two index types share the encoder shape (8-byte big-endian uint64)
so the storage layer can be type-agnostic — it just stores opaque cells.

`s2sphere` is bundled at the top level (added in Phase 1's dep bump).
"""

from __future__ import annotations

import struct
from collections.abc import Mapping
from typing import Any

import s2sphere as s2
from shapely.geometry import Point
from shapely.geometry.base import BaseGeometry

from secantus.geo import _SphericalCircle

# Default tuning for `RegionCoverer`. mongod's defaults are similar
# (server uses min=8 / max=20 with internal heuristics); we pick values
# that produce reasonable cell counts for typical query polygons.
_S2_DEFAULT_MIN_LEVEL: int = 4
_S2_DEFAULT_MAX_LEVEL: int = 16
_S2_DEFAULT_MAX_CELLS: int = 64

# Default 2d index parameters mirror mongod's defaults: 26-bit precision,
# longitude-style coordinate range [-180, 180]. Users can override via
# `bits` / `min` / `max` options.
_2D_DEFAULT_BITS: int = 26
_2D_DEFAULT_MIN: float = -180.0
_2D_DEFAULT_MAX: float = 180.0


def encode_cell(cell_id: int) -> bytes:
    """Fixed-width 8-byte big-endian uint64.

    ``S2CellId`` values are uint64 already; ``2d`` geohashes are at most
    52 bits (max 26 bits per axis × 2 axes). Encoding both as big-endian
    uint64 means a lex-byte WT cursor walk yields strictly-monotonic
    cell-ID order, so `_collect_prefix` over a cell range Just Works.
    """
    if cell_id < 0 or cell_id >= (1 << 64):
        raise ValueError(f"cell id {cell_id} out of uint64 range")
    return struct.pack(">Q", cell_id)


def decode_cell(blob: bytes) -> int:
    """Inverse of :func:`encode_cell` (debug / introspection)."""
    return struct.unpack(">Q", blob[:8])[0]


# ---------------------------------------------------------------------------
# 2dsphere: S2 cell IDs
# ---------------------------------------------------------------------------


def s2_doc_covering(geom: BaseGeometry) -> list[int]:
    """Cells covering a doc's geometry — covering cells **plus their ancestors**.

    A query at *any* level needs to be able to find the doc. The covering
    cells alone don't achieve that: an indexed cell at level 12 has a
    different byte prefix than a query Point's leaf cell (level 30) or
    a coarse query covering at level 6, even though they all "cover the
    same area". Writing each covering cell + every ancestor up to level
    0 means a query for any cell `C` that is an ancestor, descendant,
    or equal to one of the covering cells will hit at least one entry.

    Cost: ~30 entries per Point doc (level 30 → 30 ancestors), and
    `(covering_size × max_level)` for shape docs. The verifier filters
    false positives, so over-indexing is safe.
    """
    if isinstance(geom, Point):
        leaf = _s2_cell_for_lng_lat(geom.x, geom.y)
        return _cell_with_ancestors(leaf)
    region = _shapely_to_s2_region(geom)
    if region is None:
        return []
    coverer = _make_coverer()
    out: list[int] = []
    seen: set[int] = set()
    for c in coverer.get_covering(region):
        for cid in _cell_with_ancestors(c):
            if cid not in seen:
                seen.add(cid)
                out.append(cid)
    return out


def _cell_with_ancestors(cell: s2.CellId) -> list[int]:
    """All ancestor cell IDs for `cell`, including itself, down to level 0."""
    out = [cell.id()]
    parent = cell
    while parent.level() > 0:
        parent = parent.parent()
        out.append(parent.id())
    return out


def s2_query_covering(geom: BaseGeometry | _SphericalCircle) -> list[int]:
    """Cells covering the query geometry **plus their ancestors**.

    Mirrors :func:`s2_doc_covering`: at lookup time we want to match an
    indexed doc whose covering cell is at *any* level — equal to a
    query cell, an ancestor, or a descendant. With both sides emitting
    "covering cells + ancestors", a query cell ``C`` (or any of its
    ancestors) matches an indexed cell when:

      * indexed cell == ``C``                — same level, same area
      * indexed cell is an ancestor of ``C`` — doc covers a wider area
                                                that contains the query
      * ``C``'s ancestors include the indexed cell at the indexed level

    The third case requires the ancestor expansion on the query side too;
    that's what this function adds. Cost: ~30 cells per query Point,
    ~50–100 for polygons. The storage layer scans each as an exact
    point-lookup against the index entries table.

    For ``$near`` / ``$nearSphere`` (where the geom is the bounding
    cap), ancestors of the cap's covering cells handle the case where
    the indexed cells span more than the cap's coverage level.
    """
    if isinstance(geom, _SphericalCircle):
        cells = [s2.CellId(c) for c in _s2_circle_covering(geom)]
    elif isinstance(geom, Point):
        cells = [_s2_cell_for_lng_lat(geom.x, geom.y)]
    else:
        region = _shapely_to_s2_region(geom)
        if region is None:
            return []
        coverer = _make_coverer()
        cells = list(coverer.get_covering(region))
    out: list[int] = []
    seen: set[int] = set()
    for cell in cells:
        for cid in _cell_with_ancestors(cell):
            if cid not in seen:
                seen.add(cid)
                out.append(cid)
    return out


def _make_coverer() -> s2.RegionCoverer:
    coverer = s2.RegionCoverer()
    coverer.min_level = _S2_DEFAULT_MIN_LEVEL
    coverer.max_level = _S2_DEFAULT_MAX_LEVEL
    coverer.max_cells = _S2_DEFAULT_MAX_CELLS
    return coverer


def _s2_cell_for_lng_lat(lng: float, lat: float) -> s2.CellId:
    return s2.CellId.from_lat_lng(s2.LatLng.from_degrees(lat, lng))


def _shapely_to_s2_region(geom: BaseGeometry):  # type: ignore[no-untyped-def]
    """Convert a Shapely geometry into a region the coverer accepts.

    The Python port of s2sphere ships only `Cap`, `LatLngRect`, `Cell`,
    and `CellUnion` as `Region` implementations — no `Loop` / `Polygon`.
    For arbitrary polygons / lines we use the bounding `LatLngRect`,
    which **over-covers**: cells outside the polygon but inside its bbox
    enter the candidate set and get filtered by the `geo_within` /
    `geo_intersects` verifier. Correctness is preserved at the cost of
    extra candidates (acceptable for typical query polygons).
    """
    if geom.is_empty:
        return None
    return _bounding_rect(geom)


def _bounding_rect(geom: BaseGeometry):  # type: ignore[no-untyped-def]
    min_x, min_y, max_x, max_y = geom.bounds
    return s2.LatLngRect(
        s2.LatLng.from_degrees(min_y, min_x),
        s2.LatLng.from_degrees(max_y, max_x),
    )


def _s2_circle_covering(circle: _SphericalCircle) -> list[int]:
    """Cells covering a ``$centerSphere`` cap."""
    axis = s2.LatLng.from_degrees(circle.center_lat, circle.center_lng).to_point()
    cap = s2.Cap.from_axis_angle(axis, s2.Angle.from_radians(circle.radius_rad))
    coverer = _make_coverer()
    return [c.id() for c in coverer.get_covering(cap)]


# ---------------------------------------------------------------------------
# 2d: planar geohash buckets
# ---------------------------------------------------------------------------


def planar_2d_index_for_point(x: float, y: float, options: Mapping[str, Any]) -> int:
    """Bucket a planar (x, y) into a 52-bit interleaved geohash.

    Unlike S2 cells, 2d index entries are point-only — `mongod` doesn't
    support arbitrary 2d-shape covering, only points. ``$geoWithin``
    queries against a 2d index produce a covering of *their* geometry
    via :func:`planar_2d_covering` and verify candidates with
    Shapely-side `geo.geo_within`.
    """
    bits, lo_x, hi_x, lo_y, hi_y = _2d_params(options)
    bx = _bucket(x, lo_x, hi_x, bits)
    by = _bucket(y, lo_y, hi_y, bits)
    return _interleave(bx, by, bits)


def planar_2d_covering(geom: BaseGeometry, options: Mapping[str, Any]) -> tuple[int, int]:
    """Return a single coarse ``(lo, hi)`` cell-ID range that contains
    every bucket the geometry intersects.

    Kept as the single-range upper bound; the storage path now uses
    :func:`planar_2d_covering_ranges` to get a tighter list of ranges.
    The lex-byte range between the bbox corners over-covers (Z-order
    interleaving means cells outside the bbox can sort between two
    cells inside it), and the per-doc verifier (`geo.geo_within`)
    filters false positives.
    """
    bits, lo_x, hi_x, lo_y, hi_y = _2d_params(options)
    min_x, min_y, max_x, max_y = geom.bounds
    bx_lo = max(0, _bucket(min_x, lo_x, hi_x, bits))
    bx_hi = min((1 << bits) - 1, _bucket(max_x, lo_x, hi_x, bits))
    by_lo = max(0, _bucket(min_y, lo_y, hi_y, bits))
    by_hi = min((1 << bits) - 1, _bucket(max_y, lo_y, hi_y, bits))
    return (
        _interleave(bx_lo, by_lo, bits),
        _interleave(bx_hi, by_hi, bits),
    )


def planar_2d_covering_ranges(
    geom: BaseGeometry,
    options: Mapping[str, Any],
    *,
    max_ranges: int = 32,
) -> list[tuple[int, int]]:
    """Tightly cover the geometry's bucket-space bbox with up to
    ``max_ranges`` Z-order ranges.

    Improves on the single-range :func:`planar_2d_covering` by walking
    a quadtree of the bucket grid and emitting one ``(lo, hi)`` Z-range
    per power-of-2-aligned quadtree cell that lands fully inside the
    bbox. Key invariant: a 2^k × 2^k cell whose lower-left corner is
    2^k-aligned has a **contiguous** Z-order range
    (``[Z(x, y), Z(x+2^k-1, y+2^k-1)]`` with no holes), so each
    "fully inside" cell maps to exactly one tight range.
    Partial-overlap cells recurse; pure-outside cells are skipped.

    Falls back to the single coarse range (matching
    :func:`planar_2d_covering`'s historical behaviour) when the
    quadtree decomposition would produce more than ``max_ranges``
    ranges — for very tortuous bboxes the planning cost overruns the
    I/O win. The verifier on the read path filters false positives
    either way, so this is a perf-not-correctness fallback.
    """
    bits, lo_x, hi_x, lo_y, hi_y = _2d_params(options)
    min_x, min_y, max_x, max_y = geom.bounds
    bx_lo = max(0, _bucket(min_x, lo_x, hi_x, bits))
    bx_hi = min((1 << bits) - 1, _bucket(max_x, lo_x, hi_x, bits))
    by_lo = max(0, _bucket(min_y, lo_y, hi_y, bits))
    by_hi = min((1 << bits) - 1, _bucket(max_y, lo_y, hi_y, bits))

    ranges: list[tuple[int, int]] = []
    overflowed = _quadtree_cover_2d(
        0, 0, bits, bx_lo, bx_hi, by_lo, by_hi, bits, ranges, max_ranges
    )
    if overflowed or not ranges:
        # Quadtree decomposition exceeded the cap, or the bbox was so
        # degenerate that nothing landed. Fall back to one coarse
        # range — better to over-scan than spend planning cost on a
        # many-range decomposition the verifier still has to re-check.
        return [
            (
                _interleave(bx_lo, by_lo, bits),
                _interleave(bx_hi, by_hi, bits),
            )
        ]
    # Coalesce adjacent / overlapping ranges so the scanner avoids
    # redundant boundary-checks at each gap.
    ranges.sort()
    coalesced: list[tuple[int, int]] = []
    for lo, hi in ranges:
        if coalesced and lo <= coalesced[-1][1] + 1:
            coalesced[-1] = (coalesced[-1][0], max(coalesced[-1][1], hi))
        else:
            coalesced.append((lo, hi))
    return coalesced


def _quadtree_cover_2d(
    x_lo: int,
    y_lo: int,
    level: int,
    bbox_x_lo: int,
    bbox_x_hi: int,
    bbox_y_lo: int,
    bbox_y_hi: int,
    total_bits: int,
    out: list[tuple[int, int]],
    max_out: int,
) -> bool:
    """Recursively quadtree-cover the bbox; append each fully-inside
    quadtree cell's Z-range to ``out``. Returns True if the budget
    ``max_out`` was exceeded so the caller can fall back to a single
    coarse range.

    Each quadtree cell is a 2^``level`` × 2^``level`` square anchored
    at ``(x_lo, y_lo)`` with ``x_lo``/``y_lo`` multiples of
    2^``level``. That alignment is what makes the cell's Z-range
    contiguous — without it, interleaving creates holes.
    """
    cell_size = 1 << level
    x_hi = x_lo + cell_size - 1
    y_hi = y_lo + cell_size - 1
    if x_hi < bbox_x_lo or x_lo > bbox_x_hi or y_hi < bbox_y_lo or y_lo > bbox_y_hi:
        return False  # cell entirely outside bbox
    if x_lo >= bbox_x_lo and x_hi <= bbox_x_hi and y_lo >= bbox_y_lo and y_hi <= bbox_y_hi:
        # Cell entirely inside bbox — emit one range.
        out.append(
            (
                _interleave(x_lo, y_lo, total_bits),
                _interleave(x_hi, y_hi, total_bits),
            )
        )
        return False
    if level == 0:
        # 1×1 cell is always entirely-inside or entirely-outside above;
        # defensive return.
        return False
    if len(out) >= max_out:
        return True
    half = cell_size >> 1
    for dx, dy in ((0, 0), (half, 0), (0, half), (half, half)):
        if _quadtree_cover_2d(
            x_lo + dx,
            y_lo + dy,
            level - 1,
            bbox_x_lo,
            bbox_x_hi,
            bbox_y_lo,
            bbox_y_hi,
            total_bits,
            out,
            max_out,
        ):
            return True
    return False


def _2d_params(
    options: Mapping[str, Any],
) -> tuple[int, float, float, float, float]:
    bits = int(options.get("bits", _2D_DEFAULT_BITS))
    lo = float(options.get("min", _2D_DEFAULT_MIN))
    hi = float(options.get("max", _2D_DEFAULT_MAX))
    return bits, lo, hi, lo, hi


def _bucket(value: float, lo: float, hi: float, bits: int) -> int:
    if hi <= lo:
        return 0
    span = hi - lo
    norm = (value - lo) / span
    if norm <= 0.0:
        return 0
    if norm >= 1.0:
        return (1 << bits) - 1
    return int(norm * (1 << bits))


def _interleave(bx: int, by: int, bits: int) -> int:
    """Bit-interleave two `bits`-wide ints into a 2*bits geohash."""
    result = 0
    for i in range(bits):
        result |= ((bx >> i) & 1) << (2 * i)
        result |= ((by >> i) & 1) << (2 * i + 1)
    return result
