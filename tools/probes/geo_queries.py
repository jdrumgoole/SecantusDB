"""Differential-probe the geo queries against a real mongod.

`$geoWithin`, `$geoIntersects`, `$near`, `$nearSphere`, the `$geoNear`
stage, and the `2d` and `2dsphere` indexes: which documents each returns, in
what order, with what distance, and what each refuses.

Each scenario seeds one collection (documents, then indexes) and runs its
steps over it:

- a READ step is compared by its whole reply;
- a WRITE step (an index build, an insert, an update) runs over a
  collection seeded afresh, and is compared by its reply and by what the
  collection and its indexes hold afterwards.

A double is compared to nine significant digits: a distance is the end of
a chain of trigonometry, and its last digits are the library's.

Commands go over a raw connection.

    PROBE_MONGOD=mongodb://127.0.0.1:27017 \
    PROBE_SERVER=mongodb://127.0.0.1:27018 python tools/probes/geo_queries.py

`PROBE_ONLY=<substring>` runs the scenarios whose name contains it.
"""

import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from _wire import Wire  # noqa: E402
from bson import Decimal128, Int64, ObjectId  # noqa: E402
from bson.errors import InvalidBSON  # noqa: E402

DEFAULT_MONGOD = "mongodb://127.0.0.1:27041"
RUN = f"{os.getpid() % 100000}"

DROP = {"$clusterTime", "operationTime", "opTime", "electionId"}
BAD = ["x", 2.5, -1, None, True, {}, [], [1], Int64(2), 0, Decimal128("2")]

S: list[dict] = []


def scenario(name, docs, indexes, steps):
    S.append({"name": name, "docs": docs, "indexes": list(indexes), "steps": list(steps)})


def index(key, **options):
    name = options.pop("name", "_".join(f"{k}_{v}" for k, v in key.items()))
    return {"key": key, "name": name, **options}


def find(filter_, **top):
    return ("r", {"find": "c", "filter": filter_, **top})


def count(filter_, **top):
    return ("r", {"count": "c", "query": filter_, **top})


def agg(*pipeline, **top):
    return ("r", {"aggregate": "c", "pipeline": list(pipeline), "cursor": {}, **top})


def near(spec, **top):
    return agg({"$geoNear": spec}, **top)


def build(*indexes):
    return ("w", {"createIndexes": "c", "indexes": list(indexes)})


def insert(*docs):
    return ("w", {"insert": "c", "documents": list(docs)})


def update(q, u, **statement):
    return ("w", {"update": "c", "updates": [{"q": q, "u": u, **statement}]})


def point(x, y):
    return {"type": "Point", "coordinates": [x, y]}


def polygon(*rings):
    return {"type": "Polygon", "coordinates": [list(map(list, ring)) for ring in rings]}


def line(*points):
    return {"type": "LineString", "coordinates": [list(p) for p in points]}


SQUARE = [(0, 0), (3, 0), (3, 3), (0, 3), (0, 0)]
HOLE = [(1, 1), (2, 1), (2, 2), (1, 2), (1, 1)]
WIDE = [(-10, -10), (20, -10), (20, 20), (-10, 20), (-10, -10)]

# ------------------------------------------------------------------ the data

# GeoJSON and legacy shapes together, as a `2dsphere` index takes them.
SPHERE = [
    {"_id": 4, "loc": point(1, 1), "k": "a"},
    {"_id": 1, "loc": point(0, 0), "k": "b"},
    {"_id": 9, "loc": point(10, 10), "k": "a"},
    {"_id": 2, "loc": point(2.5, 2.5), "k": "b"},
    {"_id": 7, "loc": point(-73.97, 40.77), "k": "a"},
    {"_id": 3, "loc": point(-73.88, 40.78), "k": "b"},
    {"_id": 12, "loc": point(179.9, 0), "k": "a"},
    {"_id": 5, "loc": point(-179.9, 0), "k": "b"},
    {"_id": 8, "loc": point(0, 89.9), "k": "a"},
    {"_id": 6, "loc": line((0, 0), (2, 2)), "k": "b"},
    {"_id": 11, "loc": polygon(SQUARE), "k": "a"},
    {"_id": 10, "loc": polygon([(5, 5), (6, 5), (6, 6), (5, 6), (5, 5)]), "k": "b"},
    {"_id": 13, "loc": [1.5, 1.5], "k": "a"},
    {"_id": 14, "loc": {"lng": 2, "lat": 2}, "k": "b"},
    {"_id": 15, "k": "a"},
    {"_id": 16, "loc": None, "k": "b"},
    {"_id": 17, "loc": {"type": "MultiPoint", "coordinates": [[1, 1], [30, 30]]}, "k": "a"},
    {"_id": 18, "loc": point(3, 3), "k": "b"},
    {"_id": 19, "loc": point(0, 3), "k": "a"},
    {"_id": 20, "loc": point(1, 1), "k": "b"},
]
# Legacy pairs only, as a `2d` index takes them.
FLAT = [
    {"_id": 4, "loc": [1, 1], "k": "a"},
    {"_id": 1, "loc": [0, 0], "k": "b"},
    {"_id": 9, "loc": [10, 10], "k": "a"},
    {"_id": 2, "loc": [2.5, 2.5], "k": "b"},
    {"_id": 7, "loc": {"x": -73.97, "y": 40.77}, "k": "a"},
    {"_id": 3, "loc": {"lng": -73.88, "lat": 40.78}, "k": "b"},
    {"_id": 12, "loc": [179.9, 0], "k": "a"},
    {"_id": 5, "loc": [-179.9, 0], "k": "b"},
    {"_id": 8, "loc": [0, 89.9], "k": "a"},
    {"_id": 13, "loc": [1.5, 1.5], "k": "a"},
    {"_id": 15, "k": "a"},
    {"_id": 16, "loc": None, "k": "b"},
    {"_id": 17, "loc": [[1, 1], [30, 30]], "k": "a"},
    {"_id": 18, "loc": [3, 3], "k": "b"},
    {"_id": 19, "loc": [0, 3], "k": "a"},
    {"_id": 20, "loc": [1, 1], "k": "b"},
    {"_id": 21, "loc": [3, 0, 99], "k": "a"},
]

# The documents of FLAT that a `2dsphere` index takes as well: no array of
# points, and no point with a third number.
BOTH = [d for d in FLAT if d["_id"] not in (17, 21)]

WITHIN = [
    {"$box": [[0, 0], [3, 3]]},
    {"$box": [[3, 3], [0, 0]]},
    {"$box": [[-180, -90], [180, 90]]},
    {"$box": [[1, 1], [1, 1]]},
    {"$box": [{"x": 0, "y": 0}, {"x": 2, "y": 2}]},
    {"$polygon": [[0, 0], [3, 0], [3, 3], [0, 3]]},
    {"$polygon": [[0, 0], [3, 0], [0, 3]]},
    {"$polygon": [[0, 0], [3, 0], [3, 3], [0, 3], [0, 0]]},
    {"$polygon": {"a": [0, 0], "b": [3, 0], "c": [0, 3]}},
    {"$center": [[0, 0], 2]},
    {"$center": [[1, 1], 0]},
    {"$center": [[0, 0], 1.4142135623730951]},
    {"$center": [{"x": 1, "y": 1}, 1]},
    {"$centerSphere": [[0, 0], 0.05]},
    {"$centerSphere": [[0, 0], 0]},
    {"$centerSphere": [[-73.9, 40.77], 0.002]},
    {"$centerSphere": [[180, 0], 0.01]},
    {"$centerSphere": [[0, 90], 0.01]},
    {"$centerSphere": [[0, 0], 4]},
    {"$geometry": polygon(SQUARE)},
    {"$geometry": polygon(SQUARE, HOLE)},
    {"$geometry": polygon(WIDE)},
    {"$geometry": polygon([(179, -1), (-179, -1), (-179, 1), (179, 1), (179, -1)])},
    {
        "$geometry": {
            "type": "MultiPolygon",
            "coordinates": [
                [list(map(list, SQUARE))],
                [[[9, 9], [11, 9], [11, 11], [9, 11], [9, 9]]],
            ],
        }
    },
    {"$geometry": point(1, 1)},
    {"$geometry": line((0, 0), (3, 3))},
    {
        "$geometry": {
            **polygon(WIDE),
            "crs": {
                "type": "name",
                "properties": {"name": "urn:x-mongodb:crs:strictwinding:EPSG:4326"},
            },
        }
    },
]
WITHIN_BAD = [
    *BAD,
    {"$box": 1},
    {"$box": []},
    {"$box": [[0, 0]]},
    {"$box": [[0, 0], [1, 1], [2, 2]]},
    {"$box": [[0, "a"], [1, 1]]},
    {"$box": [[0], [1, 1]]},
    {"$polygon": [[0, 0], [1, 1]]},
    {"$polygon": []},
    {"$polygon": 1},
    {"$polygon": [[0, 0], [1, "a"], [2, 2]]},
    {"$center": [[0, 0]]},
    {"$center": [[0, 0], -1]},
    {"$center": [[0, 0], "a"]},
    {"$center": [[0, 0], 1, 2]},
    {"$center": 1},
    {"$centerSphere": [[0, 0], -1]},
    {"$centerSphere": [[0, 0]]},
    {"$centerSphere": [[200, 0], 1]},
    {"$centerSphere": [[0, 100], 1]},
    {"$geometry": {}},
    {"$geometry": 1},
    {"$geometry": {"type": "Polygon"}},
    {"$geometry": {"type": "Nope", "coordinates": [0, 0]}},
    {"$geometry": {"type": "Polygon", "coordinates": [[[0, 0], [3, 0], [3, 3], [0, 3]]]}},
    {"$geometry": {"type": "Polygon", "coordinates": [[[0, 0], [3, 0], [0, 0]]]}},
    {"$geometry": {"type": "Polygon", "coordinates": [[[0, 0], [3, 3], [3, 0], [0, 3], [0, 0]]]}},
    {"$geometry": {"type": "Polygon", "coordinates": [[[0, 0], [200, 0], [3, 3], [0, 0]]]}},
    {"$geometry": {"type": "Polygon", "coordinates": []}},
    {"$geometry": polygon(SQUARE), "nope": 1},
    {"$box": [[0, 0], [1, 1]], "$center": [[0, 0], 1]},
    {"$nope": 1},
    {"$uniqueDocs": True},
    {"$box": [[0, 0], [3, 3]], "$uniqueDocs": True},
]
INTERSECTS = [
    {"$geometry": point(1, 1)},
    {"$geometry": point(1.5, 1.5)},
    {"$geometry": point(0.5, 0.5)},
    {"$geometry": line((0, 3), (3, 0))},
    {"$geometry": line((0, 0), (10, 10))},
    {"$geometry": polygon(SQUARE)},
    {"$geometry": polygon(SQUARE, HOLE)},
    {"$geometry": polygon(WIDE)},
    {"$geometry": {"type": "MultiPoint", "coordinates": [[0, 0], [10, 10]]}},
    {
        "$geometry": {
            "type": "MultiLineString",
            "coordinates": [[[0, 3], [3, 0]], [[9, 9], [11, 11]]],
        }
    },
    {
        "$geometry": {
            "type": "GeometryCollection",
            "geometries": [point(0, 0), line((9, 9), (11, 11))],
        }
    },
]
INTERSECTS_BAD = [
    *BAD,
    {"$box": [[0, 0], [3, 3]]},
    {"$geometry": [1, 1]},
    {"$geometry": {}},
    {"$geometry": {"type": "Point", "coordinates": [200, 0]}},
    {"$geometry": {"type": "Point", "coordinates": [0]}},
    {"$geometry": {"type": "Point", "coordinates": "a"}},
    {"$geometry": {"type": "LineString", "coordinates": [[0, 0]]}},
    {"$geometry": point(1, 1), "nope": 1},
]
SPHERE_INDEXES = [
    ("no index", []),
    ("2dsphere", [index({"loc": "2dsphere"})]),
]
for label, indexes in SPHERE_INDEXES:
    scenario(
        f"$geoWithin, {label}",
        SPHERE,
        indexes,
        [
            *[find({"loc": {"$geoWithin": shape}}) for shape in WITHIN],
            *[find({"loc": {"$within": shape}}) for shape in WITHIN[:3]],
            *[find({"loc": {"$geoWithin": shape}, "k": "a"}) for shape in WITHIN[:6]],
            *[count({"loc": {"$geoWithin": shape}}) for shape in WITHIN[:8]],
            *[agg({"$match": {"loc": {"$geoWithin": shape}}}) for shape in WITHIN[:8]],
            *[find({"loc": {"$not": {"$geoWithin": shape}}}) for shape in WITHIN[:3]],
            *[find({"$or": [{"loc": {"$geoWithin": shape}}, {"_id": 9}]}) for shape in WITHIN[:3]],
            *[
                find({"loc": {"$geoWithin": shape}}, sort={"_id": 1}, limit=3)
                for shape in WITHIN[:6]
            ],
        ],
    )
    scenario(
        f"$geoWithin refused, {label}",
        SPHERE,
        indexes,
        [find({"loc": {"$geoWithin": shape}}) for shape in WITHIN_BAD],
    )
    scenario(
        f"$geoIntersects, {label}",
        SPHERE,
        indexes,
        [
            *[find({"loc": {"$geoIntersects": shape}}) for shape in INTERSECTS],
            *[find({"loc": {"$geoIntersects": shape}, "k": "b"}) for shape in INTERSECTS[:5]],
            *[count({"loc": {"$geoIntersects": shape}}) for shape in INTERSECTS[:5]],
            *[find({"loc": {"$geoIntersects": shape}}) for shape in INTERSECTS_BAD],
        ],
    )
scenario(
    "$geoWithin, 2d",
    FLAT,
    [index({"loc": "2d"})],
    [
        *[find({"loc": {"$geoWithin": shape}}) for shape in WITHIN[:19]],
        *[find({"loc": {"$geoWithin": shape}, "k": "a"}) for shape in WITHIN[:6]],
        *[count({"loc": {"$geoWithin": shape}}) for shape in WITHIN[:8]],
        *[find({"loc": {"$geoWithin": shape}}) for shape in WITHIN_BAD],
    ],
)
scenario(
    "$geoWithin, flat documents and no index",
    FLAT,
    [],
    [find({"loc": {"$geoWithin": shape}}) for shape in WITHIN[:24]],
)

# -------------------------------------------------------- $near, $nearSphere

NEAR = [
    {"$near": {"$geometry": point(0, 0)}},
    {"$near": {"$geometry": point(1, 1)}},
    {"$near": {"$geometry": point(0, 0), "$maxDistance": 200000}},
    {"$near": {"$geometry": point(0, 0), "$minDistance": 200000}},
    {"$near": {"$geometry": point(0, 0), "$minDistance": 100000, "$maxDistance": 400000}},
    {"$near": {"$geometry": point(0, 0), "$maxDistance": 0}},
    {"$near": {"$geometry": point(180, 0), "$maxDistance": 50000}},
    {"$near": {"$geometry": point(-73.9, 40.77), "$maxDistance": 10000}},
    {"$nearSphere": {"$geometry": point(0, 0)}},
    {"$nearSphere": {"$geometry": point(0, 0), "$maxDistance": 200000}},
    {"$nearSphere": {"$geometry": point(0, 0), "$minDistance": 200000}},
    {"$near": [0, 0]},
    {"$near": [0, 0], "$maxDistance": 3},
    {"$near": [0, 0], "$maxDistance": 0.05},
    {"$near": [0, 0], "$minDistance": 3},
    {"$near": {"x": 0, "y": 0}},
    {"$nearSphere": [0, 0]},
    {"$nearSphere": [0, 0], "$maxDistance": 0.05},
    {"$nearSphere": [0, 0], "$minDistance": 0.05},
    {"$nearSphere": [-73.9, 40.77], "$maxDistance": 0.002},
    {"$geoNear": [0, 0]},
]
NEAR_BAD = [
    *[{"$near": v} for v in BAD],
    *[{"$nearSphere": v} for v in BAD],
    {"$near": {"$geometry": point(0, 0), "$maxDistance": -1}},
    {"$near": {"$geometry": point(0, 0), "$maxDistance": "a"}},
    {"$near": {"$geometry": point(0, 0), "$minDistance": -1}},
    {"$near": {"$geometry": point(0, 0), "nope": 1}},
    {"$near": {"$geometry": line((0, 0), (1, 1))}},
    {"$near": {"$geometry": polygon(SQUARE)}},
    {"$near": {"$geometry": point(200, 0)}},
    {"$near": {"$geometry": {}}},
    {"$near": [0, 0], "$maxDistance": -1},
    {"$near": [0, 0], "$maxDistance": "a"},
    {"$near": [0, 0], "nope": 1},
    {"$near": [0]},
    {"$near": [0, "a"]},
    {"$near": [200, 0]},
    {"$nearSphere": [200, 0]},
    {"$nearSphere": [0, 100]},
    {"$near": [0, 0], "$nearSphere": [0, 0]},
    {"$near": [0, 0], "$geoWithin": {"$box": [[0, 0], [1, 1]]}},
    {"$near": [0, 0], "$eq": 1},
]


def near_steps(shapes):
    return [
        *[find({"loc": shape}) for shape in shapes],
        *[find({"loc": shape, "k": "a"}) for shape in shapes[:6]],
        *[find({"loc": shape}, limit=3) for shape in shapes[:6]],
        *[find({"loc": shape}, skip=2, limit=3) for shape in shapes[:6]],
        *[find({"loc": shape}, sort={"_id": 1}) for shape in shapes[:4]],
        *[find({"loc": shape}, projection={"_id": 1}) for shape in shapes[:4]],
        *[count({"loc": shape}) for shape in shapes[:4]],
        *[agg({"$match": {"loc": shape}}) for shape in shapes[:4]],
        *[find({"$or": [{"loc": shape}, {"_id": 1}]}) for shape in shapes[:2]],
        *[find({"$and": [{"loc": shape}, {"k": "a"}]}) for shape in shapes[:2]],
        *[find({"$nor": [{"loc": shape}]}) for shape in shapes[:2]],
        *[find({"loc": {"$not": shape}}) for shape in shapes[:2]],
        *[find({"loc": shape, "other": shape}) for shape in shapes[:2]],
        *[("r", {"distinct": "c", "key": "k", "query": {"loc": shape}}) for shape in shapes[:2]],
    ]


scenario("$near, 2dsphere", SPHERE, [index({"loc": "2dsphere"})], near_steps(NEAR))
scenario("$near, 2d", FLAT, [index({"loc": "2d"})], near_steps(NEAR))
scenario("$near, no index", SPHERE, [], near_steps(NEAR)[:30])
scenario(
    "$near, a 2d and a 2dsphere index",
    BOTH,
    [index({"loc": "2d"}), index({"loc": "2dsphere"}, name="s")],
    near_steps(NEAR)[:30],
)
scenario(
    "$near, a compound 2dsphere index",
    SPHERE,
    [index({"loc": "2dsphere", "k": 1})],
    near_steps(NEAR)[:30],
)
scenario(
    "$near refused, 2dsphere",
    SPHERE,
    [index({"loc": "2dsphere"})],
    [find({"loc": shape}) for shape in NEAR_BAD],
)
scenario(
    "$near refused, 2d",
    FLAT,
    [index({"loc": "2d"})],
    [find({"loc": shape}) for shape in NEAR_BAD],
)
scenario(
    "$near in a write",
    SPHERE,
    [index({"loc": "2dsphere"})],
    [
        ("w", {"delete": "c", "deletes": [{"q": {"loc": NEAR[0]}, "limit": 1}]}),
        ("w", {"delete": "c", "deletes": [{"q": {"loc": NEAR[2]}, "limit": 0}]}),
        update({"loc": NEAR[0]}, {"$set": {"hit": 1}}),
        update({"loc": NEAR[2]}, {"$set": {"hit": 1}}, multi=True),
        update({"loc": NEAR[2]}, {"$set": {"hit": 1}}, upsert=True),
        ("w", {"findAndModify": "c", "query": {"loc": NEAR[1]}, "update": {"$set": {"hit": 1}}}),
        ("w", {"findAndModify": "c", "query": {"loc": NEAR[1]}, "remove": True}),
    ],
)

# ------------------------------------------------------------------ $geoNear

GEONEAR = [
    {"near": point(0, 0), "distanceField": "d"},
    {"near": point(0, 0), "distanceField": "d", "spherical": True},
    {"near": point(0, 0), "distanceField": "d", "spherical": False},
    {"near": point(0, 0), "distanceField": "d", "maxDistance": 200000},
    {"near": point(0, 0), "distanceField": "d", "minDistance": 200000},
    {"near": point(0, 0), "distanceField": "d", "minDistance": 100000, "maxDistance": 400000},
    {"near": point(0, 0), "distanceField": "d", "query": {"k": "a"}},
    {"near": point(0, 0), "distanceField": "d", "includeLocs": "where"},
    {"near": point(0, 0), "distanceField": "d", "distanceMultiplier": 0.001},
    {"near": point(0, 0), "distanceField": "d.deep"},
    {"near": point(0, 0), "distanceField": "k"},
    {"near": point(0, 0), "distanceField": "d", "key": "loc"},
    {"near": point(-73.9, 40.77), "distanceField": "d", "maxDistance": 10000},
    {"near": point(180, 0), "distanceField": "d", "maxDistance": 50000},
    {"near": [0, 0], "distanceField": "d"},
    {"near": [0, 0], "distanceField": "d", "spherical": True},
    {"near": [0, 0], "distanceField": "d", "spherical": True, "maxDistance": 0.05},
    {"near": [0, 0], "distanceField": "d", "maxDistance": 3},
    {"near": [0, 0], "distanceField": "d", "spherical": True, "distanceMultiplier": 6378.1},
    {"near": {"x": 0, "y": 0}, "distanceField": "d"},
    {"near": point(0, 0)},
    {"near": point(0, 0), "distanceField": "d", "limit": 2},
    {"near": point(0, 0), "distanceField": "d", "num": 2},
    {"near": point(0, 0), "distanceField": "d", "uniqueDocs": True},
]
GEONEAR_BAD = [
    *BAD,
    {"distanceField": "d"},
    {"near": point(0, 0), "distanceField": 1},
    {"near": point(0, 0), "distanceField": ""},
    {"near": point(0, 0), "distanceField": "$d"},
    {"near": "x", "distanceField": "d"},
    {"near": 1, "distanceField": "d"},
    {"near": [0], "distanceField": "d"},
    {"near": [0, "a"], "distanceField": "d"},
    {"near": point(200, 0), "distanceField": "d"},
    {"near": line((0, 0), (1, 1)), "distanceField": "d"},
    {"near": point(0, 0), "distanceField": "d", "maxDistance": -1},
    {"near": point(0, 0), "distanceField": "d", "maxDistance": "a"},
    {"near": point(0, 0), "distanceField": "d", "minDistance": -1},
    {"near": point(0, 0), "distanceField": "d", "spherical": "a"},
    {"near": point(0, 0), "distanceField": "d", "spherical": 1},
    {"near": point(0, 0), "distanceField": "d", "query": 1},
    {"near": point(0, 0), "distanceField": "d", "query": {"$nope": 1}},
    {"near": point(0, 0), "distanceField": "d", "query": {"loc": {"$near": [0, 0]}}},
    {"near": point(0, 0), "distanceField": "d", "key": 1},
    {"near": point(0, 0), "distanceField": "d", "key": ""},
    {"near": point(0, 0), "distanceField": "d", "key": "nope"},
    {"near": point(0, 0), "distanceField": "d", "includeLocs": 1},
    {"near": point(0, 0), "distanceField": "d", "distanceMultiplier": -1},
    {"near": point(0, 0), "distanceField": "d", "distanceMultiplier": "a"},
    {"near": point(0, 0), "distanceField": "d", "nope": 1},
]


def geonear_steps(specs):
    return [
        *[near(spec) for spec in specs],
        *[agg({"$geoNear": spec}, {"$limit": 3}) for spec in specs[:6]],
        *[agg({"$geoNear": spec}, {"$match": {"k": "b"}}) for spec in specs[:3]],
        *[agg({"$geoNear": spec}, {"$project": {"d": 1}}) for spec in specs[:3]],
        *[agg({"$geoNear": spec}, {"$sort": {"_id": 1}}) for spec in specs[:3]],
        *[agg({"$geoNear": spec}, {"$count": "n"}) for spec in specs[:6]],
        *[agg({"$match": {"k": "a"}}, {"$geoNear": spec}) for spec in specs[:2]],
        *[agg({"$geoNear": spec}, {"$geoNear": spec}) for spec in specs[:1]],
        *[agg({"$facet": {"x": [{"$geoNear": spec}]}}) for spec in specs[:1]],
        *[
            agg(
                {"$lookup": {"from": "c", "pipeline": [{"$geoNear": spec}], "as": "x"}},
                {"$limit": 1},
            )
            for spec in specs[:1]
        ],
        *[
            agg({"$unionWith": {"coll": "c", "pipeline": [{"$geoNear": spec}]}}, {"$count": "n"})
            for spec in specs[:1]
        ],
    ]


scenario("$geoNear, 2dsphere", SPHERE, [index({"loc": "2dsphere"})], geonear_steps(GEONEAR))
scenario("$geoNear, 2d", FLAT, [index({"loc": "2d"})], geonear_steps(GEONEAR))
scenario("$geoNear, no index", SPHERE, [], geonear_steps(GEONEAR)[:30])
scenario(
    "$geoNear, a 2d and a 2dsphere index",
    BOTH,
    [index({"loc": "2d"}), index({"loc": "2dsphere"}, name="s")],
    geonear_steps(GEONEAR)[:30],
)
scenario(
    "$geoNear, two 2dsphere indexes",
    [{**d, "at": d["loc"]} if "loc" in d else d for d in SPHERE],
    [index({"loc": "2dsphere"}), index({"at": "2dsphere"})],
    [
        *geonear_steps(GEONEAR)[:14],
        near({"near": point(0, 0), "distanceField": "d", "key": "at"}),
        near({"near": point(0, 0), "distanceField": "d", "key": "loc", "maxDistance": 200000}),
    ],
)
scenario(
    "$geoNear refused, 2dsphere",
    SPHERE,
    [index({"loc": "2dsphere"})],
    [near(spec) for spec in GEONEAR_BAD],
)
scenario(
    "$geoNear refused, 2d",
    FLAT,
    [index({"loc": "2d"})],
    [near(spec) for spec in GEONEAR_BAD],
)

# ------------------------------------------------------------- the indexes

VALUES = [
    point(1, 1),
    [1, 1],
    {"lng": 1, "lat": 1},
    {"x": 1, "y": 1},
    line((0, 0), (1, 1)),
    polygon(SQUARE),
    polygon(SQUARE, HOLE),
    {"type": "MultiPoint", "coordinates": [[0, 0], [1, 1]]},
    {"type": "GeometryCollection", "geometries": [point(0, 0)]},
    [[1, 1], [2, 2]],
    [point(1, 1), point(2, 2)],
    None,
    [],
    {},
    "x",
    5,
    True,
    [1],
    [1, "a"],
    [1, 2, 3],
    [200, 0],
    [0, 100],
    [-180, -90],
    [180, 90],
    point(200, 0),
    point(0, 100),
    {"type": "Point", "coordinates": [1]},
    {"type": "Point", "coordinates": "a"},
    {"type": "Point"},
    {"type": "Nope", "coordinates": [1, 1]},
    {"coordinates": [1, 1]},
    {"type": "Polygon", "coordinates": [[[0, 0], [3, 0], [3, 3], [0, 3]]]},
    {"type": "Polygon", "coordinates": [[[0, 0], [3, 3], [3, 0], [0, 3], [0, 0]]]},
    {"type": "LineString", "coordinates": [[0, 0]]},
    {"type": "LineString", "coordinates": [[0, 0], [0, 0]]},
    {"a": 1, "b": 2},
    {"x": "a", "y": 1},
    {"x": 1},
]
for kind in ("2dsphere", "2d"):
    scenario(
        f"{kind}: what a document may hold",
        [{"_id": 0, "k": 1}],
        [index({"loc": kind})],
        [
            *[insert({"_id": 1, "loc": v}) for v in VALUES],
            *[update({"_id": 0}, {"$set": {"loc": v}}) for v in VALUES],
            *[insert({"_id": 1, "loc": {"deep": v}}) for v in VALUES[:6]],
        ],
    )
    for at, v in enumerate(VALUES):
        scenario(
            f"{kind}: a build over value {at}",
            [{"_id": 0, "k": 1}, {"_id": 1, "loc": v}],
            [],
            [build(index({"loc": kind}))],
        )
scenario(
    "the index options",
    SPHERE[:5],
    [],
    [
        build(index({"loc": "2dsphere"})),
        build(index({"loc": "2dsphere", "k": 1})),
        build(index({"k": 1, "loc": "2dsphere"})),
        build(index({"loc": "2dsphere", "at": "2dsphere"})),
        *[
            build(index({"loc": "2dsphere"}, **{"2dsphereIndexVersion": v}))
            for v in [1, 2, 3, 4, 0, "a", 2.5]
        ],
        build(index({"loc": "2dsphere"}, unique=True)),
        build(index({"loc": "2dsphere"}, sparse=True)),
        build(index({"loc": "2dsphere"}, partialFilterExpression={"k": "a"})),
        build(index({"loc": "2dsphere"}, bits=26)),
        build(index({"loc": "2dsphere"}, min=-10)),
        build(index({"loc": "2dsphere"}, expireAfterSeconds=10)),
        build(index({"loc": "2dsphere"}, collation={"locale": "en"})),
        build(index({"loc": "2dsphere"}), index({"loc": "2dsphere"}, name="again")),
        build(index({"loc": "2dsphere"}), index({"loc": 1})),
        build(index({"loc.coordinates": "2dsphere"})),
        *[
            build(index({"loc": v}))
            for v in ["2dsphere", "2d", "2D", "2dSphere", "geoHaystack", "nope", ""]
        ],
    ],
)
scenario(
    "the 2d index options",
    FLAT[:5],
    [],
    [
        build(index({"loc": "2d"})),
        build(index({"loc": "2d", "k": 1})),
        build(index({"k": 1, "loc": "2d"})),
        build(index({"loc": "2d", "at": "2d"})),
        *[build(index({"loc": "2d"}, bits=v)) for v in [1, 26, 32, 33, 0, -1, "a", 2.5]],
        *[
            build(index({"loc": "2d"}, min=lo, max=hi))
            for lo, hi in [(-10, 10), (0, 1), (10, -10), (-1, 300), ("a", 1), (0, 0)]
        ],
        build(index({"loc": "2d"}, unique=True)),
        build(index({"loc": "2d"}, sparse=True)),
        build(index({"loc": "2d"}, partialFilterExpression={"k": "a"})),
        build(index({"loc": "2d"}, **{"2dsphereIndexVersion": 3})),
        build(index({"loc": "2d"}, expireAfterSeconds=10)),
        build(index({"loc": "2d"}), index({"loc": "2dsphere"})),
        build(index({"loc": "2d"}, min=-10, max=10), index({"k": 1})),
    ],
)
scenario(
    "a 2d index with its own range",
    [{"_id": 1, "loc": [1, 1]}, {"_id": 2, "loc": [9, 9]}, {"_id": 3, "loc": [-9, 9]}],
    [index({"loc": "2d"}, min=-10, max=10)],
    [
        insert({"_id": 9, "loc": [10, 0]}),
        insert({"_id": 9, "loc": [9.999, 0]}),
        insert({"_id": 9, "loc": [-10, 0]}),
        insert({"_id": 9, "loc": [11, 0]}),
        find({"loc": {"$geoWithin": {"$box": [[0, 0], [10, 10]]}}}),
        find({"loc": {"$geoWithin": {"$box": [[0, 0], [20, 20]]}}}),
        find({"loc": {"$near": [0, 0]}}),
        find({"loc": {"$near": [20, 0]}}),
        find({"loc": {"$geoWithin": {"$center": [[0, 0], 50]}}}),
    ],
)
scenario(
    "the indexes a query may use",
    SPHERE,
    [index({"loc": "2dsphere"}), index({"k": 1})],
    [
        find({"loc": {"$geoWithin": {"$geometry": polygon(SQUARE)}}}, hint="loc_2dsphere"),
        find({"loc": {"$geoWithin": {"$geometry": polygon(SQUARE)}}}, hint="k_1"),
        find({"loc": {"$geoWithin": {"$geometry": polygon(SQUARE)}}}, hint={"$natural": 1}),
        find({"loc": {"$near": {"$geometry": point(0, 0)}}}, hint="loc_2dsphere"),
        find({"loc": {"$near": {"$geometry": point(0, 0)}}}, hint="k_1"),
        find({"loc": {"$near": {"$geometry": point(0, 0)}}}, hint={"$natural": 1}),
        find({"loc": {"$near": {"$geometry": point(0, 0)}}}, hint="nope"),
        find({"k": "a"}, hint="loc_2dsphere"),
        find({}, hint="loc_2dsphere"),
        find({}, sort={"loc": 1}),
        find({"loc": point(1, 1)}),
        find({"loc": [1.5, 1.5]}),
        find({"loc.coordinates": [1, 1]}),
        find({"loc": {"$exists": True}}, hint="loc_2dsphere"),
        near({"near": point(0, 0), "distanceField": "d"}, hint="k_1"),
        near({"near": point(0, 0), "distanceField": "d"}, hint="loc_2dsphere"),
    ],
)

# ------------------------------------------------------------------ running

UUID_TEXT = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")
DURATION_TEXT = re.compile(r"durationMillis: \d+ms")


def clean(value, db, key=None):
    if isinstance(value, dict):
        return {k: clean(v, db, k) for k, v in value.items() if k not in DROP}
    if isinstance(value, list):
        return [clean(v, db, key) for v in value]
    if isinstance(value, ObjectId):
        return "OID"
    if isinstance(value, Int64):
        # An open cursor's id is the server's own choice.
        return "CURSOR" if key == "id" and int(value) != 0 else f"Int64({int(value)})"
    if isinstance(value, float):
        return "NaN" if value != value else f"double({value:.9g})"
    if isinstance(value, str):
        # An index build that fails says how long its scan had run.
        text = DURATION_TEXT.sub("durationMillis: Nms", value.replace(db, "@db"))
        return UUID_TEXT.sub("UUID", text)
    return value


def send(wire, uri, db, command):
    """One command; a connection that fails is replaced and reported."""
    try:
        return wire, wire.command(db, command)
    except TimeoutError:
        wire.close()
        return Wire(uri), {"probe": "TIMEOUT"}
    except InvalidBSON as exc:
        return wire, {"probe": f"REPLY NOT DECODED {exc}"}
    except (ConnectionError, OSError) as exc:
        wire.close()
        return Wire(uri), {"probe": f"CONNECTION {type(exc).__name__}"}


def seed(wire, uri, db, spec):
    """Drop `c` and build it again; the reply that went wrong, if one did."""
    wire, _ = send(wire, uri, db, {"drop": "c"})
    wire, made = send(wire, uri, db, {"insert": "c", "documents": spec["docs"]})
    if made.get("n") != len(spec["docs"]) or made.get("writeErrors"):
        return wire, made
    if spec["indexes"]:
        wire, made = send(wire, uri, db, {"createIndexes": "c", "indexes": spec["indexes"]})
        if made.get("ok") != 1.0:
            return wire, made
    return wire, None


def held(wire, uri, db):
    """What the collection holds: its documents and its indexes."""
    wire, found = send(wire, uri, db, {"find": "c", "batchSize": 500})
    docs = found.get("cursor", {}).get("firstBatch", found)
    wire, listed = send(wire, uri, db, {"listIndexes": "c"})
    indexes = [
        {k: v for k, v in spec.items() if k not in ("v", "ns")}
        for spec in listed.get("cursor", {}).get("firstBatch", [])
    ]
    return wire, {"docs": docs, "indexes": sorted(indexes, key=lambda spec: spec.get("name", ""))}


def run_scenario(uri, number, spec):
    db = f"geo{RUN}_{number}"
    wire = Wire(uri)
    out = []
    wire, failed = seed(wire, uri, db, spec)
    dirty = False
    for kind, command in spec["steps"]:
        if failed is None and (dirty or kind == "w"):
            wire, failed = seed(wire, uri, db, spec)
            dirty = False
        if failed is not None:
            out.append({"probe": "SETUP", "reply": clean(failed, db)})
            failed, dirty = None, True
            continue
        wire, reply = send(wire, uri, db, command)
        if kind == "r":
            out.append({"reply": clean(reply, db)})
            continue
        dirty = True
        wire, after = held(wire, uri, db)
        out.append({"reply": clean(reply, db), "after": clean(after, db)})
    wire, _ = send(wire, uri, db, {"dropDatabase": 1})
    wire.close()
    return out


def show(step):
    _, command = step
    name = next(iter(command))
    rest = {k: v for k, v in command.items() if k not in (name, "cursor")}
    return f"{name} {rest!r}"


def self_check(uri):
    """The reference must answer the three kinds of query as the probe assumes."""
    spec = {
        "docs": [{"_id": 1, "loc": point(5, 5)}, {"_id": 2, "loc": point(1, 1)}],
        "indexes": [index({"loc": "2dsphere"})],
        "steps": [
            find({"loc": {"$geoWithin": {"$geometry": polygon(SQUARE)}}}),
            find({"loc": {"$near": {"$geometry": point(0, 0)}}}),
            near({"near": point(0, 0), "distanceField": "d"}),
        ],
    }
    got = run_scenario(uri, 0, spec)
    ids = [[d["_id"] for d in g["reply"].get("cursor", {}).get("firstBatch", [])] for g in got]
    if ids != [[2], [2, 1], [2, 1]] or "d" not in got[2]["reply"]["cursor"]["firstBatch"][0]:
        print("self-check failed on mongod:", got, file=sys.stderr)
        return False
    return True


def main():
    mongod_uri = os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD)
    server_uri = os.environ.get("PROBE_SERVER")
    if not server_uri:
        print("set PROBE_SERVER to the server under test", file=sys.stderr)
        return 2
    only = os.environ.get("PROBE_ONLY")
    width = int(os.environ.get("PROBE_WIDTH", "900"))
    if not self_check(mongod_uri):
        return 2

    divergent = replies = replies_bad = total = 0
    for number, spec in enumerate(S, start=1):
        name, steps = spec["name"], spec["steps"]
        if only and only not in name:
            continue
        total += 1
        want = run_scenario(mongod_uri, number, spec)
        got = run_scenario(server_uri, number, spec)
        replies += len(steps)
        setups = [w for w in want if w.get("probe") == "SETUP"]
        if setups:
            # The seed is the probe's own: mongod refusing it is a fault in
            # the probe, and nothing run after it says anything.
            print(f"{name}: mongod refused the seed: {str(setups[0])[:width]}", file=sys.stderr)
            return 2
        bad = [i for i, (w, g) in enumerate(zip(want, got, strict=True)) if repr(w) != repr(g)]
        if not bad:
            continue
        divergent += 1
        replies_bad += len(bad)
        print(f"\n--- {name} ({len(bad)} of {len(steps)})")
        for i in bad:
            print(f"  step {i}: {show(steps[i])[:width]}")
            for part in ("reply", "after", "probe"):
                w, g = want[i].get(part), got[i].get(part)
                if repr(w) != repr(g):
                    print(f"    mongod {part}: {str(w)[:width]}")
                    print(f"    server {part}: {str(g)[:width]}")
    print(
        f"\n=== geo queries: {total} scenarios, {replies} replies -- "
        f"{divergent} scenarios and {replies_bad} replies differ ==="
    )
    return 1 if divergent else 0


if __name__ == "__main__":
    sys.exit(main())
