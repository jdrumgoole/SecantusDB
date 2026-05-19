"""End-to-end pymongo tests for the geo operators.

Insert real GeoJSON / legacy-pair docs through the wire, run finds and
aggregates with each geo operator, assert correct membership and
ordering. Mirrors the shape of ``tests/test_aggregate.py``.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer):
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield mc
    finally:
        mc.close()


def _geo_point(lng: float, lat: float) -> dict:
    return {"type": "Point", "coordinates": [lng, lat]}


def test_geo_within_box_filters_legacy_pairs(client: MongoClient) -> None:
    coll = client["geo"]["pairs"]
    coll.insert_many(
        [
            {"_id": 1, "loc": [0.0, 0.0]},
            {"_id": 2, "loc": [5.0, 5.0]},
            {"_id": 3, "loc": [50.0, 50.0]},
        ]
    )
    found = list(coll.find({"loc": {"$geoWithin": {"$box": [[-1, -1], [10, 10]]}}}))
    assert {d["_id"] for d in found} == {1, 2}


def test_geo_within_polygon_geojson(client: MongoClient) -> None:
    coll = client["geo"]["geojson"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _geo_point(2.0, 2.0)},
            {"_id": 2, "loc": _geo_point(15.0, 15.0)},
        ]
    )
    poly = {
        "type": "Polygon",
        "coordinates": [[[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]]],
    }
    found = list(coll.find({"loc": {"$geoWithin": {"$geometry": poly}}}))
    assert [d["_id"] for d in found] == [1]


def test_geo_within_center_sphere(client: MongoClient) -> None:
    coll = client["geo"]["sphere"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _geo_point(0.0, 0.0)},
            {"_id": 2, "loc": _geo_point(0.0001, 0.0001)},  # ~15 m away
            {"_id": 3, "loc": _geo_point(1.0, 1.0)},  # ~157 km away
        ]
    )
    # 0.001 rad ≈ 6.4 km
    found = list(coll.find({"loc": {"$geoWithin": {"$centerSphere": [[0.0, 0.0], 0.001]}}}))
    assert {d["_id"] for d in found} == {1, 2}


def test_geo_intersects_polygon(client: MongoClient) -> None:
    coll = client["geo"]["intersects"]
    coll.insert_many(
        [
            {
                "_id": 1,
                "loc": {
                    "type": "Polygon",
                    "coordinates": [[[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]]],
                },
            },
            {
                "_id": 2,
                "loc": {
                    "type": "Polygon",
                    "coordinates": [[[100, 100], [110, 100], [110, 110], [100, 110], [100, 100]]],
                },
            },
        ]
    )
    query = {
        "type": "Polygon",
        "coordinates": [[[5, 5], [15, 5], [15, 15], [5, 15], [5, 5]]],
    }
    found = list(coll.find({"loc": {"$geoIntersects": {"$geometry": query}}}))
    assert [d["_id"] for d in found] == [1]


def test_geo_intersects_rejects_non_geojson(client: MongoClient) -> None:
    coll = client["geo"]["bad_intersects"]
    coll.insert_one({"_id": 1, "loc": [0.0, 0.0]})
    # mongod requires $geometry under $geoIntersects; we mirror.
    with pytest.raises(OperationFailure):
        list(coll.find({"loc": {"$geoIntersects": {"$box": [[0, 0], [1, 1]]}}}))


def test_near_geojson_with_max_distance_in_meters(client: MongoClient) -> None:
    coll = client["geo"]["near_geojson"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _geo_point(0.0, 0.0)},
            {"_id": 2, "loc": _geo_point(0.001, 0.0)},  # ~111 m
            {"_id": 3, "loc": _geo_point(1.0, 0.0)},  # ~111 km — outside
        ]
    )
    found = list(
        coll.find(
            {
                "loc": {
                    "$near": {
                        "$geometry": _geo_point(0.0, 0.0),
                        "$maxDistance": 200,  # meters
                    }
                }
            }
        )
    )
    assert {d["_id"] for d in found} == {1, 2}


def test_near_legacy_pair_with_max(client: MongoClient) -> None:
    coll = client["geo"]["near_legacy"]
    coll.insert_many(
        [
            {"_id": 1, "loc": [0.0, 0.0]},
            {"_id": 2, "loc": [3.0, 4.0]},  # planar distance 5
            {"_id": 3, "loc": [50.0, 50.0]},
        ]
    )
    # Legacy: $near accepts [x, y, max] where max is in input units.
    found = list(coll.find({"loc": {"$near": [0.0, 0.0, 6.0]}}))
    assert {d["_id"] for d in found} == {1, 2}


def test_near_with_min_distance_excludes_self(client: MongoClient) -> None:
    coll = client["geo"]["near_min"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _geo_point(0.0, 0.0)},
            {"_id": 2, "loc": _geo_point(0.001, 0.0)},  # ~111 m
        ]
    )
    found = list(
        coll.find(
            {
                "loc": {
                    "$near": {
                        "$geometry": _geo_point(0.0, 0.0),
                        "$minDistance": 50,  # 50 m floor → excludes _id 1
                        "$maxDistance": 500,
                    }
                }
            }
        )
    )
    assert {d["_id"] for d in found} == {2}


def test_geo_near_aggregation_attaches_distance(client: MongoClient) -> None:
    coll = client["geo"]["agg_near"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _geo_point(0.0, 0.0), "name": "origin"},
            {"_id": 2, "loc": _geo_point(0.001, 0.0), "name": "near"},
            {"_id": 3, "loc": _geo_point(1.0, 0.0), "name": "far"},
        ]
    )
    pipeline = [
        {
            "$geoNear": {
                "near": _geo_point(0.0, 0.0),
                "distanceField": "distance",
                "key": "loc",
                "maxDistance": 500,  # 500 m
            }
        }
    ]
    docs = list(coll.aggregate(pipeline))
    # Origin first (distance 0), then "near" — "far" beyond 500m is dropped.
    assert [d["_id"] for d in docs] == [1, 2]
    assert docs[0]["distance"] == pytest.approx(0.0, abs=1e-6)
    assert 100 < docs[1]["distance"] < 120  # ~111.3 m


def test_geo_near_with_query_prefilter(client: MongoClient) -> None:
    coll = client["geo"]["agg_near_filter"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _geo_point(0.0, 0.0), "category": "A"},
            {"_id": 2, "loc": _geo_point(0.001, 0.0), "category": "B"},
            {"_id": 3, "loc": _geo_point(0.002, 0.0), "category": "A"},
        ]
    )
    pipeline = [
        {
            "$geoNear": {
                "near": _geo_point(0.0, 0.0),
                "distanceField": "d",
                "key": "loc",
                "query": {"category": "A"},
            }
        }
    ]
    docs = list(coll.aggregate(pipeline))
    assert [d["_id"] for d in docs] == [1, 3]


def test_geo_near_include_locs_attaches_raw_geojson(client: MongoClient) -> None:
    """`includeLocs` echoes back the raw doc geometry under the named field.

    Each output doc gets a copy of whatever was stored — GeoJSON shape
    in, GeoJSON shape out — so the client can plot the matched points
    without a second round-trip through the doc.
    """
    coll = client["geo"]["include_locs_geojson"]
    docs = [
        {"_id": 1, "loc": _geo_point(0.0, 0.0), "name": "origin"},
        {"_id": 2, "loc": _geo_point(0.001, 0.0), "name": "east"},
    ]
    coll.insert_many(docs)
    pipeline = [
        {
            "$geoNear": {
                "near": _geo_point(0.0, 0.0),
                "distanceField": "d",
                "key": "loc",
                "includeLocs": "matchedLoc",
                "maxDistance": 500,
            }
        }
    ]
    out = list(coll.aggregate(pipeline))
    assert [d["_id"] for d in out] == [1, 2]
    # Each output doc carries an exact copy of the stored geometry.
    assert out[0]["matchedLoc"] == _geo_point(0.0, 0.0)
    assert out[1]["matchedLoc"] == _geo_point(0.001, 0.0)
    # And the originals aren't disturbed.
    assert out[0]["loc"] == _geo_point(0.0, 0.0)


def test_geo_near_include_locs_with_legacy_pair(client: MongoClient) -> None:
    """Legacy ``[x, y]`` pairs round-trip as pairs (not converted to GeoJSON)."""
    coll = client["geo"]["include_locs_legacy"]
    coll.insert_many(
        [
            {"_id": 1, "loc": [0.0, 0.0]},
            {"_id": 2, "loc": [3.0, 4.0]},
        ]
    )
    pipeline = [
        {
            "$geoNear": {
                "near": [0.0, 0.0],
                "distanceField": "d",
                "key": "loc",
                "includeLocs": "where",
            }
        }
    ]
    out = list(coll.aggregate(pipeline))
    assert out[0]["where"] == [0.0, 0.0]
    assert out[1]["where"] == [3.0, 4.0]


def test_geo_near_include_locs_dotted_path(client: MongoClient) -> None:
    """``includeLocs`` accepts dotted paths and creates the nested object."""
    coll = client["geo"]["include_locs_dotted"]
    coll.insert_one({"_id": 1, "loc": _geo_point(1.0, 2.0)})
    out = list(
        coll.aggregate(
            [
                {
                    "$geoNear": {
                        "near": _geo_point(0.0, 0.0),
                        "distanceField": "d",
                        "key": "loc",
                        "includeLocs": "match.loc",
                    }
                }
            ]
        )
    )
    assert out[0]["match"] == {"loc": _geo_point(1.0, 2.0)}


def test_geo_near_include_locs_must_be_string(client: MongoClient) -> None:
    """Type check on ``includeLocs`` mirrors mongod (errors on non-string)."""
    coll = client["geo"]["include_locs_bad"]
    coll.insert_one({"_id": 1, "loc": _geo_point(0.0, 0.0)})
    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$geoNear": {
                            "near": _geo_point(0.0, 0.0),
                            "distanceField": "d",
                            "key": "loc",
                            "includeLocs": 42,  # not a string
                        }
                    }
                ]
            )
        )


def test_geo_near_planar_distance(client: MongoClient) -> None:
    coll = client["geo"]["agg_near_planar"]
    coll.insert_many(
        [
            {"_id": 1, "loc": [0.0, 0.0]},
            {"_id": 2, "loc": [3.0, 4.0]},  # planar 5
        ]
    )
    pipeline = [
        {
            "$geoNear": {
                "near": [0.0, 0.0],  # Legacy pair → planar by default
                "distanceField": "d",
                "key": "loc",
            }
        }
    ]
    docs = list(coll.aggregate(pipeline))
    assert docs[0]["d"] == pytest.approx(0.0)
    assert docs[1]["d"] == pytest.approx(5.0)


def test_geo_within_uncoercible_doc_excluded(client: MongoClient) -> None:
    # A doc with garbage in the geo field should silently not match —
    # mongod tolerates bad stored geometry by skipping it; we mirror.
    coll = client["geo"]["bad_doc"]
    coll.insert_many(
        [
            {"_id": 1, "loc": "not a geometry"},
            {"_id": 2, "loc": [0.0, 0.0]},
        ]
    )
    found = list(coll.find({"loc": {"$geoWithin": {"$box": [[-1, -1], [1, 1]]}}}))
    assert [d["_id"] for d in found] == [2]


def test_unknown_geo_operator_errors(client: MongoClient) -> None:
    coll = client["geo"]["unknown_op"]
    coll.insert_one({"_id": 1, "loc": [0.0, 0.0]})
    with pytest.raises(OperationFailure):
        list(coll.find({"loc": {"$geoBogus": {"$box": [[-1, -1], [1, 1]]}}}))


def test_geo_near_errors_without_key_and_without_geo_index(
    client: MongoClient,
) -> None:
    coll = client["geo"]["no_index_no_key"]
    coll.insert_one({"_id": 1, "loc": _geo_point(0.0, 0.0)})
    # Real mongod also requires either `key` or a geo index — the auto-
    # infer path can't produce a key out of thin air.
    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$geoNear": {
                            "near": _geo_point(0.0, 0.0),
                            "distanceField": "d",
                        }
                    }
                ]
            )
        )


def test_geo_near_infers_key_from_2dsphere_index(client: MongoClient) -> None:
    coll = client["geo"]["infer_2dsphere"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _geo_point(0.0, 0.0)},
            {"_id": 2, "loc": _geo_point(0.001, 0.0)},  # ~111 m
            {"_id": 3, "loc": _geo_point(1.0, 0.0)},  # ~111 km
        ]
    )
    coll.create_index([("loc", "2dsphere")])
    docs = list(
        coll.aggregate(
            [
                {
                    "$geoNear": {
                        "near": _geo_point(0.0, 0.0),
                        "distanceField": "d",
                        "maxDistance": 200,
                        # `key` omitted — picker should infer "loc".
                    }
                }
            ]
        )
    )
    assert [d["_id"] for d in docs] == [1, 2]


def test_geo_near_infers_key_from_2d_index(client: MongoClient) -> None:
    coll = client["geo"]["infer_2d"]
    coll.insert_many(
        [
            {"_id": 1, "pos": [0.0, 0.0]},
            {"_id": 2, "pos": [3.0, 4.0]},  # planar distance 5
            {"_id": 3, "pos": [50.0, 50.0]},
        ]
    )
    coll.create_index([("pos", "2d")])
    docs = list(
        coll.aggregate(
            [
                {
                    "$geoNear": {
                        "near": [0.0, 0.0],
                        "distanceField": "d",
                        "maxDistance": 6.0,
                        # `key` omitted — picker should infer "pos".
                    }
                }
            ]
        )
    )
    assert [d["_id"] for d in docs] == [1, 2]


def test_geo_near_explicit_key_overrides_index_inference(
    client: MongoClient,
) -> None:
    # Two geo fields with indexes — the explicit `key` wins over the
    # picker's first-found choice. Verifies `key` isn't silently
    # ignored when an index also exists.
    coll = client["geo"]["override"]
    coll.insert_many(
        [
            {
                "_id": 1,
                "home": _geo_point(0.0, 0.0),
                "work": _geo_point(50.0, 50.0),
            },
        ]
    )
    coll.create_index([("home", "2dsphere")])
    coll.create_index([("work", "2dsphere")])
    docs = list(
        coll.aggregate(
            [
                {
                    "$geoNear": {
                        "near": _geo_point(50.0, 50.0),
                        "distanceField": "d",
                        "key": "work",  # explicit
                    }
                }
            ]
        )
    )
    assert docs[0]["d"] == pytest.approx(0.0, abs=1e-6)


def test_near_legacy_sibling_max_distance(client: MongoClient) -> None:
    """Mongod's legacy 2d shape lifts ``$maxDistance`` to the parent
    condition (the Java driver's ``Filters.near(field, x, y, max, min)``
    builds exactly this). SecantusDB must accept both the nested form
    (above) and this sibling form."""
    coll = client["geo"]["near_sibling_max"]
    coll.insert_many(
        [
            {"_id": 1, "loc": [0.0, 0.0]},
            {"_id": 2, "loc": [3.0, 4.0]},  # planar dist 5
            {"_id": 3, "loc": [50.0, 50.0]},
        ]
    )
    found = list(coll.find({"loc": {"$near": [0.0, 0.0], "$maxDistance": 6.0}}))
    assert {d["_id"] for d in found} == {1, 2}


def test_near_legacy_sibling_min_and_max(client: MongoClient) -> None:
    """Both $maxDistance and $minDistance at sibling level — annulus
    between the two."""
    coll = client["geo"]["near_sibling_min_max"]
    coll.insert_many(
        [
            {"_id": 1, "loc": [0.0, 0.0]},
            {"_id": 2, "loc": [3.0, 4.0]},  # dist 5
            {"_id": 3, "loc": [6.0, 8.0]},  # dist 10
            {"_id": 4, "loc": [50.0, 50.0]},
        ]
    )
    found = list(
        coll.find(
            {
                "loc": {
                    "$near": [0.0, 0.0],
                    "$minDistance": 3.0,
                    "$maxDistance": 9.0,
                }
            }
        )
    )
    # Only _id 2 (dist 5) is inside the (3, 9) annulus.
    assert {d["_id"] for d in found} == {2}


def test_near_sphere_legacy_sibling_max(client: MongoClient) -> None:
    """Same sibling-level shape, ``$nearSphere`` flavour. The Java
    driver's ``Filters.nearSphere(field, x, y, max, min)`` against a
    legacy 2d coordinate field generates this."""
    coll = client["geo"]["near_sphere_sibling"]
    coll.insert_many(
        [
            {"_id": 1, "loc": [0.0, 0.0]},
            {"_id": 2, "loc": [0.01, 0.0]},  # ~1100 m in spherical
            {"_id": 3, "loc": [50.0, 50.0]},
        ]
    )
    # Mongod convention: $nearSphere with a legacy coord pair takes
    # $maxDistance in RADIANS (unit-sphere measure), not meters.
    # 0.0002 rad ≈ 1275 m on Earth — covers _id 1 (0 m) and _id 2
    # (~1100 m). _id 3 way outside.
    found = list(coll.find({"loc": {"$nearSphere": [0.0, 0.0], "$maxDistance": 0.0002}}))
    assert {d["_id"] for d in found} == {1, 2}
