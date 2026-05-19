# Geospatial

SecantusDB ships full geo support — operators, the `$geoNear`
aggregation stage, and both `2d` and `2dsphere` index acceleration.
This page is the operator-by-operator and index-by-index reference;
the [Indexes](indexes.md) page covers the rest of the index
machinery.

## Operators

| Operator | Doc-side data | Notes |
|---|---|---|
| `$geoWithin` | GeoJSON, legacy `[x, y]`, `{x, y}`, `{lng, lat}` | Containment test. Accepts `$geometry` (GeoJSON), `$box`, `$polygon`, `$center` (planar disk), `$centerSphere` (great-circle cap) |
| `$geoIntersects` | GeoJSON | `$geometry` only — mongod restricts to GeoJSON |
| `$near` | GeoJSON or legacy pair | Containment + sort by distance. `$maxDistance` / `$minDistance` are bounds; without `$maxDistance` against a geo index, falls through to full scan |
| `$nearSphere` | GeoJSON or legacy pair | Same as `$near` but always spherical; legacy form treats `$maxDistance` as radians on the unit sphere (mongod's convention) |

All four are reachable through pymongo, mongo-go-driver,
mongo-node-driver, mongo-java-driver (`Filters.geoWithin` /
`Filters.geoIntersects` / `Filters.near` / `Filters.nearSphere`), and
mongo-ruby-driver. The mongo-java-driver gauge's
`GeoJsonFiltersFunctionalSpecification` and
`GeoFiltersFunctionalSpecification` exercise the full driver-side
`Filters` builder path against SecantusDB and pass 10/10.

## `$geoNear` aggregation stage

```python
pipeline = [
    {
        "$geoNear": {
            "near": {"type": "Point", "coordinates": [0.0, 0.0]},
            "distanceField": "distance",
            "key": "loc",
            "maxDistance": 500,        # meters (GeoJSON)
            "query": {"category": "A"}, # pre-filter
            "includeLocs": "matchedLoc", # echo raw doc geometry under this field
        }
    }
]
```

`$geoNear` auto-picks a `2d` or `2dsphere` index on the named field
when one exists; falls back to a full-scan distance computation
otherwise. Output is sorted ascending by distance and `distanceField`
carries the value. `includeLocs` echoes the raw doc geometry under a
named field so the client can plot the matched points without a
second round-trip.

## Index types

### `2dsphere` — modern spherical

```python
coll.create_index([("loc", "2dsphere")])
```

Best for GeoJSON data and any computation that should be
geodesically correct on a sphere. Doc-side geometries must be valid
GeoJSON (or legacy pairs interpreted as `[lng, lat]`).

Implementation:

- Each indexed geometry's S2 cell covering is computed via
  `s2sphere.RegionCoverer` (min level 4, max level 16, max 64 cells),
  and every covering cell **plus every ancestor back to level 0** is
  written as an index entry. The ancestor expansion is what lets a
  query at any level — a coarse covering, a leaf point cell, anything
  in between — find the doc.
- Query-side coverings expand the same way. The storage layer does
  exact point-lookups against the entries table; Shapely (planar) and
  haversine (spherical) verify candidates.
- Cell IDs are encoded as fixed-width 8-byte big-endian uint64 so the
  WT B-tree's lex byte ordering aligns with S2 cell-ID ordering.

### `2d` — legacy planar

```python
coll.create_index([("loc", "2d")])
```

For legacy `[x, y]` coordinate pairs (lng/lat by default). Useful
when working with non-geographic 2D data (game-world positions, plot
coordinates, etc.) where the spherical assumption is wrong.

Implementation:

- Each indexed point gets one bit-interleaved geohash entry at the
  configured precision (`bits`, default 26; `min` / `max`, default
  -180 / 180).
- Query-side: the bbox is decomposed into a list of tight Z-order
  ranges via a quadtree (each 2^k × 2^k power-of-2-aligned cell that
  lands fully inside the bbox yields one contiguous Z-range). Falls
  back to a single coarse range over `max_ranges=32` for very
  tortuous bboxes. The Shapely / haversine verifier filters false
  positives.
- 2d indexes are point-only on the doc side — mongod itself doesn't
  index arbitrary shapes against a 2d index.

### Compound geo + scalar

```python
coll.create_index([("loc", "2dsphere"), ("category", 1)])
```

The geo column drives the cell-covering scan; the trailing scalar
column gets filtered at the verifier step. Useful when most queries
combine a geo predicate with a category / status filter — the
combined index cuts down the scan from "all geo matches" to "geo
matches in category X."

### Custom 2d range

```python
coll.create_index([("pos", "2d")], min=0, max=1000, bits=20)
```

Override the default lng / lat range when storing non-geographic
coords. `bits` sets the geohash precision per axis (1–32; default
26). The grid is `2^bits × 2^bits` buckets.

## Distance units — the gotcha

Three different conventions are in play depending on the spec shape
and index type. SecantusDB matches mongod's rules:

| Spec shape | Operator | `$maxDistance` unit |
|---|---|---|
| GeoJSON `$geometry` | `$near` / `$nearSphere` | Meters (great-circle on Earth) |
| Legacy `[x, y]` pair | `$near` | Input units (planar Pythagoras) |
| Legacy `[x, y]` pair | `$nearSphere` | Radians on the unit sphere |

The legacy + spherical case is the most surprising — the bound is in
**radians**, not meters. To convert: meters / 6_378_100 ≈ radians.

Internally, SecantusDB's `distance(spherical=True)` returns meters
(Earth-radius scaled), so the matcher converts legacy+spherical
bounds via `* EARTH_RADIUS_METERS`. The 2d-index picker for the same
shape converts via `* 180 / π` to get degrees (matching mongod's
behaviour against a 2d index).

This isn't usually a problem if you use the GeoJSON form everywhere
(unambiguously meters). The legacy forms only come up when a driver
builder API like Java's `Filters.nearSphere(field, x, y, max, min)`
serializes to the legacy shape on the wire.

## Doc-side geometry shapes accepted

| Shape | Example | Notes |
|---|---|---|
| GeoJSON `Point` | `{type: "Point", coordinates: [lng, lat]}` | The canonical form for both index types |
| GeoJSON `Polygon` | `{type: "Polygon", coordinates: [[[lng, lat], …]]}` | 2dsphere only (2d indexes don't index polygons) |
| GeoJSON `LineString` | `{type: "LineString", coordinates: [[lng, lat], …]}` | 2dsphere only |
| Legacy `[x, y]` pair | `[10.0, 20.0]` | Treated as `[lng, lat]` for 2dsphere |
| Legacy `{x, y}` map | `{x: 10.0, y: 20.0}` | Treated as `[lng, lat]` |
| Legacy `{lng, lat}` map | `{lng: 10.0, lat: 20.0}` | Explicit aliases |

Malformed geometries reject at insert / update / upsert /
createIndex time with mongod's documented code `16572`
(`"Can't extract geo keys"`). Stored bad geometry is tolerated by
the operators (treated as "no match") without raising — mirrors
mongod.

## Worked example

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as srv:
    client = MongoClient(srv.uri)
    coll = client["app"]["places"]

    # Insert some restaurants in central London.
    coll.insert_many([
        {"_id": 1, "name": "The Fox",  "loc": {"type": "Point", "coordinates": [-0.1276, 51.5072]}},
        {"_id": 2, "name": "Borough",  "loc": {"type": "Point", "coordinates": [-0.0900, 51.5050]}},
        {"_id": 3, "name": "Camden",   "loc": {"type": "Point", "coordinates": [-0.1426, 51.5395]}},
        {"_id": 4, "name": "Greenwich","loc": {"type": "Point", "coordinates": [ 0.0098, 51.4769]}},
    ])
    coll.create_index([("loc", "2dsphere")])

    # All restaurants within 5 km of Trafalgar Square.
    cursor = coll.find({
        "loc": {
            "$near": {
                "$geometry": {"type": "Point", "coordinates": [-0.1280, 51.5080]},
                "$maxDistance": 5000,  # meters
            }
        }
    })
    for doc in cursor:
        print(doc["name"])

    # Same query as aggregation with distance attached.
    pipeline = [{
        "$geoNear": {
            "near": {"type": "Point", "coordinates": [-0.1280, 51.5080]},
            "distanceField": "metres",
            "key": "loc",
            "maxDistance": 5000,
        }
    }]
    for doc in coll.aggregate(pipeline):
        print(f"{doc['name']}: {doc['metres']:.0f} m")

    # $geoWithin a polygon.
    westminster = {
        "type": "Polygon",
        "coordinates": [[
            [-0.14, 51.49], [-0.12, 51.49],
            [-0.12, 51.52], [-0.14, 51.52],
            [-0.14, 51.49],
        ]],
    }
    inside = list(coll.find({"loc": {"$geoWithin": {"$geometry": westminster}}}))
```

## Validation coverage

| Surface | Tests |
|---|---|
| Unit tests (parser, distance, containment) | `tests/test_geo.py` — 30 tests |
| Operator integration via pymongo | `tests/test_geo_query.py` — 24 tests |
| Index acceleration + explain | `tests/test_geo_index.py` — 25 tests |
| Cross-driver smoke (mongosh / node / go) | `tests/test_geo_cross_driver.py` — 3 tests |
| pymongo conformance gauge | `test_collection.py`'s built-in geo tests at 100% |
| mongo-java-driver gauge | `:driver-core:test` runs `GeoJsonFiltersFunctionalSpecification` + `GeoFiltersFunctionalSpecification` at 10/10 |

## Out of scope

- **Exact mongod error-string matching** — we surface mongod's error
  codes (`16572` for bad geo extraction, `2` for bad operator args)
  but not the exact `errmsg` wording. Driver tests that pin specific
  English strings fall here; tests that key on the code pass.
- **Geo-haystack indexes** — deprecated in MongoDB 5.0; no point.
- **The `geoSearch` command** — superseded by `$geoNear`.

## Where the code lives

- [`src/secantus/geo.py`](https://github.com/jdrumgoole/SecantusDB/blob/main/src/secantus/geo.py)
  — geometry primitives, GeoJSON parsing, Shapely / haversine
  distance + containment. Pure module, no storage import.
- [`src/secantus/geo_index.py`](https://github.com/jdrumgoole/SecantusDB/blob/main/src/secantus/geo_index.py)
  — S2 cell coverings (2dsphere), bit-interleaved geohash + quadtree
  Z-order range decomposition (2d), cell-ID encoding for the WT
  entries table.
- [`src/secantus/storage.py`](https://github.com/jdrumgoole/SecantusDB/blob/main/src/secantus/storage.py)
  `_pick_geo_index_for_filter` / `_try_geo_index_id_keys` /
  `_geo_query_cells` — picker integration.
- [`src/secantus/query.py`](https://github.com/jdrumgoole/SecantusDB/blob/main/src/secantus/query.py)
  `_op_geo_within` / `_op_geo_intersects` / `_op_geo_near` /
  `_parse_near_spec` — operator matcher (also handles the legacy
  mongod sibling-form `$maxDistance` / `$minDistance`).
- [`src/secantus/aggregate.py`](https://github.com/jdrumgoole/SecantusDB/blob/main/src/secantus/aggregate.py)
  `_stage_geoNear` — the aggregation stage.
