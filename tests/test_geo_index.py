"""Phase 2 tests: 2dsphere / 2d index acceleration.

Verifies index creation, multi-cell entry writes, picker selection,
result equivalence with full-scan, and explain reporting.
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


def _pt(lng: float, lat: float) -> dict:
    return {"type": "Point", "coordinates": [lng, lat]}


# --- Index creation ---------------------------------------------------------


def test_create_2dsphere_index(server: SecantusDBServer, client: MongoClient) -> None:
    coll = client["geo_idx"]["sphere"]
    coll.insert_one({"_id": 1, "loc": _pt(0.0, 0.0)})
    name = coll.create_index([("loc", "2dsphere")])
    assert name == "loc_2dsphere"
    indexes = list(coll.list_indexes())
    target = next(ix for ix in indexes if ix["name"] == "loc_2dsphere")
    assert target["key"] == {"loc": "2dsphere"}
    # mongod's listIndexes carries no multikey flag (probed 6.0.16) — the
    # internal one that makes the regular pickers skip geo indexes is
    # catalog state, asserted at the storage layer.
    assert server.storage.index_is_multikey("geo_idx", "sphere", "loc_2dsphere") is True


def test_create_2d_index(client: MongoClient) -> None:
    coll = client["geo_idx"]["planar"]
    coll.insert_one({"_id": 1, "loc": [0.0, 0.0]})
    name = coll.create_index([("loc", "2d")])
    assert name == "loc_2d"


def test_unique_geo_index_rejected(client: MongoClient) -> None:
    coll = client["geo_idx"]["unique_geo"]
    coll.insert_one({"_id": 1, "loc": _pt(0.0, 0.0)})
    # mongod rejects unique on a geo index — same here.
    with pytest.raises(OperationFailure):
        coll.create_index([("loc", "2dsphere")], unique=True)


# --- Result equivalence: index path == full-scan path ----------------------


def _seed_sphere_dataset(coll) -> None:
    coll.insert_many(
        [
            {"_id": 1, "loc": _pt(0.0, 0.0)},
            {"_id": 2, "loc": _pt(0.001, 0.0)},  # ~111 m east
            {"_id": 3, "loc": _pt(0.001, 0.001)},  # ~157 m NE
            {"_id": 4, "loc": _pt(1.0, 1.0)},  # ~157 km NE
            {"_id": 5, "loc": _pt(50.0, 50.0)},  # far away
        ]
    )


def test_2dsphere_geo_within_index_matches_scan(client: MongoClient) -> None:
    coll = client["geo_idx"]["match"]
    _seed_sphere_dataset(coll)
    coll.create_index([("loc", "2dsphere")])
    q = {"loc": {"$geoWithin": {"$centerSphere": [[0.0, 0.0], 0.001]}}}
    indexed = sorted(d["_id"] for d in coll.find(q))
    scanned = sorted(d["_id"] for d in coll.find(q, hint="$natural"))
    assert indexed == scanned
    # Sanity — should pull in the close points but not the far ones.
    assert 4 not in indexed
    assert 5 not in indexed


def test_2dsphere_near_index_matches_scan(client: MongoClient) -> None:
    coll = client["geo_idx"]["near_match"]
    _seed_sphere_dataset(coll)
    coll.create_index([("loc", "2dsphere")])
    q = {
        "loc": {
            "$near": {
                "$geometry": _pt(0.0, 0.0),
                "$maxDistance": 200,  # 200 m
            }
        }
    }
    indexed = sorted(d["_id"] for d in coll.find(q))
    scanned = sorted(d["_id"] for d in coll.find(q, hint="$natural"))
    assert indexed == scanned


def test_2dsphere_geo_intersects_polygon(client: MongoClient) -> None:
    coll = client["geo_idx"]["intersects"]
    coll.insert_many(
        [
            {
                "_id": 1,
                "loc": {
                    "type": "Polygon",
                    "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]],
                },
            },
            {
                "_id": 2,
                "loc": {
                    "type": "Polygon",
                    "coordinates": [[[10, 10], [11, 10], [11, 11], [10, 11], [10, 10]]],
                },
            },
        ]
    )
    coll.create_index([("loc", "2dsphere")])
    query_geom = {
        "type": "Polygon",
        "coordinates": [[[0.5, 0.5], [2, 0.5], [2, 2], [0.5, 2], [0.5, 0.5]]],
    }
    found = sorted(
        d["_id"] for d in coll.find({"loc": {"$geoIntersects": {"$geometry": query_geom}}})
    )
    assert found == [1]


def test_2d_geo_within_box_index(client: MongoClient) -> None:
    coll = client["geo_idx"]["planar_box"]
    coll.insert_many(
        [
            {"_id": 1, "loc": [1.0, 1.0]},
            {"_id": 2, "loc": [5.0, 5.0]},
            {"_id": 3, "loc": [50.0, 50.0]},
        ]
    )
    coll.create_index([("loc", "2d")])
    q = {"loc": {"$geoWithin": {"$box": [[0.0, 0.0], [10.0, 10.0]]}}}
    indexed = sorted(d["_id"] for d in coll.find(q))
    scanned = sorted(d["_id"] for d in coll.find(q, hint="$natural"))
    assert indexed == scanned
    assert indexed == [1, 2]


# --- Explain reporting ------------------------------------------------------


def test_explain_reports_ixscan_for_geo_within(client: MongoClient) -> None:
    coll = client["geo_idx"]["explain"]
    coll.insert_one({"_id": 1, "loc": _pt(0.0, 0.0)})
    coll.create_index([("loc", "2dsphere")])
    explain = client["geo_idx"].command(
        {
            "explain": {
                "find": "explain",
                "filter": {"loc": {"$geoWithin": {"$centerSphere": [[0.0, 0.0], 0.001]}}},
            }
        }
    )
    plan = explain["queryPlanner"]["winningPlan"]
    # FETCH wraps an IXSCAN inputStage when the plan is index-driven.
    assert plan["stage"] == "FETCH"
    inner = plan["inputStage"]
    assert inner["stage"] == "IXSCAN"
    assert inner["indexName"] == "loc_2dsphere"
    assert inner["keyPattern"] == {"loc": "2dsphere"}


def test_explain_reports_collscan_without_geo_index(client: MongoClient) -> None:
    coll = client["geo_idx"]["no_idx"]
    coll.insert_one({"_id": 1, "loc": _pt(0.0, 0.0)})
    explain = client["geo_idx"].command(
        {
            "explain": {
                "find": "no_idx",
                "filter": {"loc": {"$geoWithin": {"$centerSphere": [[0.0, 0.0], 0.001]}}},
            }
        }
    )
    plan = explain["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "COLLSCAN"


# --- Update / delete maintain index entries --------------------------------


def test_update_moves_doc_in_geo_index(client: MongoClient) -> None:
    coll = client["geo_idx"]["update"]
    coll.insert_one({"_id": 1, "loc": _pt(0.0, 0.0)})
    coll.create_index([("loc", "2dsphere")])
    # Move the doc somewhere far via replacement-style update.
    coll.replace_one({"_id": 1}, {"_id": 1, "loc": _pt(50.0, 50.0)})
    # Should NOT be in the small disk anymore.
    assert coll.find_one({"loc": {"$geoWithin": {"$centerSphere": [[0, 0], 0.001]}}}) is None
    # SHOULD be in a far disk.
    assert coll.find_one({"loc": {"$geoWithin": {"$centerSphere": [[50, 50], 0.001]}}}) == {
        "_id": 1,
        "loc": _pt(50.0, 50.0),
    }


def test_delete_removes_geo_entries(client: MongoClient) -> None:
    coll = client["geo_idx"]["delete"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _pt(0.0, 0.0)},
            {"_id": 2, "loc": _pt(0.0001, 0.0001)},
        ]
    )
    coll.create_index([("loc", "2dsphere")])
    coll.delete_one({"_id": 1})
    found = sorted(
        d["_id"] for d in coll.find({"loc": {"$geoWithin": {"$centerSphere": [[0, 0], 0.001]}}})
    )
    assert found == [2]


def test_drop_index_clears_entries(client: MongoClient) -> None:
    coll = client["geo_idx"]["drop_idx"]
    coll.insert_one({"_id": 1, "loc": _pt(0.0, 0.0)})
    coll.create_index([("loc", "2dsphere")])
    coll.drop_index("loc_2dsphere")
    indexes = [ix["name"] for ix in coll.list_indexes()]
    assert "loc_2dsphere" not in indexes
    # Query still works (full-scan).
    assert (
        coll.find_one({"loc": {"$geoWithin": {"$centerSphere": [[0.0, 0.0], 0.001]}}}) is not None
    )


# --- Polygon doc geometry writes multiple cell entries ---------------------


def test_polygon_doc_writes_multi_cell_entries(client: MongoClient) -> None:
    coll = client["geo_idx"]["multi_cell"]
    # A polygon spanning ~10° lng/lat won't fit a single cell at the
    # default cover levels — it'll be many cells. Verify by checking
    # that querying a small sub-disk inside the polygon's interior
    # still finds the doc (proving the cell covering reached that area).
    coll.insert_one(
        {
            "_id": 1,
            "loc": {
                "type": "Polygon",
                "coordinates": [[[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]]],
            },
        }
    )
    coll.create_index([("loc", "2dsphere")])
    # Query a 1-meter disk in the middle of the polygon.
    found = list(coll.find({"loc": {"$geoIntersects": {"$geometry": _pt(5.0, 5.0)}}}))
    assert [d["_id"] for d in found] == [1]


# --- Input validation: bad geometry rejected at write time -----------------


def test_2dsphere_rejects_out_of_range_longitude(client: MongoClient) -> None:
    coll = client["geo_idx"]["bad_lng"]
    coll.create_index([("loc", "2dsphere")])
    with pytest.raises(OperationFailure) as exc:
        coll.insert_one({"_id": 1, "loc": _pt(200.0, 0.0)})  # lng > 180
    assert exc.value.code == 16572


def test_2dsphere_rejects_out_of_range_latitude(client: MongoClient) -> None:
    coll = client["geo_idx"]["bad_lat"]
    coll.create_index([("loc", "2dsphere")])
    with pytest.raises(OperationFailure) as exc:
        coll.insert_one({"_id": 1, "loc": _pt(0.0, 95.0)})  # lat > 90
    assert exc.value.code == 16572


def test_2dsphere_rejects_unparseable_geometry(client: MongoClient) -> None:
    coll = client["geo_idx"]["bad_shape"]
    coll.create_index([("loc", "2dsphere")])
    with pytest.raises(OperationFailure) as exc:
        coll.insert_one({"_id": 1, "loc": "not a geometry"})
    assert exc.value.code == 16572


def test_2dsphere_missing_field_is_sparse(client: MongoClient) -> None:
    """Missing or null indexed field must NOT error — geo indexes are
    sparse-by-default (matches mongod). The doc just isn't indexed.
    """
    coll = client["geo_idx"]["sparse_missing"]
    coll.create_index([("loc", "2dsphere")])
    coll.insert_one({"_id": 1, "name": "no_loc"})  # field missing
    coll.insert_one({"_id": 2, "loc": None})  # field explicitly null
    coll.insert_one({"_id": 3, "loc": _pt(0.0, 0.0)})
    # All three docs were stored.
    assert coll.count_documents({}) == 3
    # Only doc 3 is in the index — confirmed by a $geoWithin query.
    hits = sorted(
        d["_id"] for d in coll.find({"loc": {"$geoWithin": {"$centerSphere": [[0, 0], 1.0]}}})
    )
    assert hits == [3]


def test_2d_rejects_polygon_doc(client: MongoClient) -> None:
    """The 2d index only supports point-typed values; a polygon must
    be rejected (not silently skipped, which was the pre-validation
    behavior)."""
    coll = client["geo_idx"]["bad_2d_shape"]
    coll.create_index([("loc", "2d")])
    with pytest.raises(OperationFailure) as exc:
        coll.insert_one(
            {
                "_id": 1,
                "loc": {
                    "type": "Polygon",
                    "coordinates": [[[0, 0], [1, 0], [1, 1], [0, 1], [0, 0]]],
                },
            }
        )
    assert exc.value.code == 16572


def test_2d_respects_custom_min_max(client: MongoClient) -> None:
    coll = client["geo_idx"]["custom_2d"]
    # Tight 2d bounds: only [-10, 10] is valid.
    coll.create_index([("loc", "2d")], min=-10, max=10)
    coll.insert_one({"_id": 1, "loc": [5.0, 5.0]})  # in bounds
    with pytest.raises(OperationFailure) as exc:
        coll.insert_one({"_id": 2, "loc": [50.0, 50.0]})  # out of bounds
    assert exc.value.code == 16572


def test_create_index_fails_on_existing_bad_doc(client: MongoClient) -> None:
    """Creating a geo index over a collection where a doc already has
    out-of-range coordinates should fail — mongod errors on the same
    'Can't extract geo keys' path during the initial scan.
    """
    coll = client["geo_idx"]["existing_bad"]
    # Insert a doc with bad coords *before* the geo index exists, so
    # validation only kicks in at create_index time.
    coll.insert_one({"_id": 1, "loc": _pt(200.0, 0.0)})
    with pytest.raises(OperationFailure) as exc:
        coll.create_index([("loc", "2dsphere")])
    assert exc.value.code == 16572
    # The index must NOT have been created.
    names = {ix["name"] for ix in coll.list_indexes()}
    assert "loc_2dsphere" not in names


def test_update_to_bad_geometry_rejected(client: MongoClient) -> None:
    """An update that swaps a valid geometry for an out-of-range one
    must fail; the doc stays at its pre-update value."""
    coll = client["geo_idx"]["update_bad"]
    coll.insert_one({"_id": 1, "loc": _pt(0.0, 0.0)})
    coll.create_index([("loc", "2dsphere")])
    with pytest.raises(OperationFailure) as exc:
        coll.replace_one({"_id": 1}, {"_id": 1, "loc": _pt(200.0, 0.0)})
    assert exc.value.code == 16572
    # Original doc is untouched and still indexed.
    found = coll.find_one({"_id": 1})
    assert found["loc"] == _pt(0.0, 0.0)


# --- Compound geo + scalar indexes -----------------------------------------


def test_compound_2dsphere_scalar_index_creates(client: MongoClient) -> None:
    """``createIndex({loc: '2dsphere', cat: 1})`` accepts trailing scalar
    fields. The index name embeds both fields, mongod-style."""
    coll = client["geo_idx"]["compound_create"]
    coll.insert_one({"_id": 1, "loc": _pt(0.0, 0.0), "cat": "park"})
    name = coll.create_index([("loc", "2dsphere"), ("cat", 1)])
    assert name == "loc_2dsphere_cat_1"
    spec = next(ix for ix in coll.list_indexes() if ix["name"] == name)
    assert spec["key"] == {"loc": "2dsphere", "cat": 1}


def test_compound_geo_scalar_filters_by_trailing_field(client: MongoClient) -> None:
    """Compound geo+scalar query results match the same filter run via
    `$natural` scan — the geo index is consulted for the cell range,
    the trailing scalar predicate is then applied at the verifier
    step.

    This is correct behaviour today because:
      1. ``_pick_geo_index_for_filter`` recognises the geo operator on
         the leading field and routes through the geo cell scan.
      2. ``find_matching`` re-applies the full filter (including the
         scalar) via ``matches()`` on the candidate set.

    The trailing scalar is *not* used as an index pre-filter (real
    mongod does that for cardinality wins); for typical small result
    sets the verifier filter is cheap. Documented in
    ``tasks/backlog.md``.
    """
    coll = client["geo_idx"]["compound_filter"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _pt(0.0001, 0.0), "cat": "restaurant"},
            {"_id": 2, "loc": _pt(0.0001, 0.0), "cat": "park"},
            {"_id": 3, "loc": _pt(0.0001, 0.0), "cat": "restaurant"},
            {"_id": 4, "loc": _pt(50.0, 50.0), "cat": "restaurant"},
        ]
    )
    coll.create_index([("loc", "2dsphere"), ("cat", 1)])
    q = {
        "cat": "restaurant",
        "loc": {"$geoWithin": {"$centerSphere": [[0, 0], 0.001]}},
    }
    indexed = sorted(d["_id"] for d in coll.find(q))
    scanned = sorted(d["_id"] for d in coll.find(q, hint="$natural"))
    assert indexed == scanned == [1, 3]


def test_geo_only_query_against_compound_index(client: MongoClient) -> None:
    """A query on just the geo field still works against a compound
    geo+scalar index — the trailing scalar is treated as "any value"."""
    coll = client["geo_idx"]["compound_geo_only"]
    coll.insert_many(
        [
            {"_id": 1, "loc": _pt(0.0001, 0.0), "cat": "park"},
            {"_id": 2, "loc": _pt(0.0001, 0.0), "cat": "restaurant"},
            {"_id": 3, "loc": _pt(50.0, 50.0), "cat": "park"},
        ]
    )
    coll.create_index([("loc", "2dsphere"), ("cat", 1)])
    found = sorted(
        d["_id"] for d in coll.find({"loc": {"$geoWithin": {"$centerSphere": [[0, 0], 0.001]}}})
    )
    assert found == [1, 2]


def test_compound_geo_scalar_input_validation_still_strict(
    client: MongoClient,
) -> None:
    """Bad geo coords on a doc with a compound index still rejects on
    the geo extractor — the trailing scalar doesn't bypass validation."""
    coll = client["geo_idx"]["compound_bad_input"]
    coll.create_index([("loc", "2dsphere"), ("cat", 1)])
    with pytest.raises(OperationFailure) as exc:
        coll.insert_one({"_id": 1, "loc": _pt(200.0, 0.0), "cat": "park"})
    assert exc.value.code == 16572


def test_geo_near_index_optimization_matches_scan(client: MongoClient) -> None:
    """A leading ``$geoNear`` with a ``maxDistance`` rides the geo index (via a
    conservative ``$geoWithin`` candidate fetch) instead of scanning the whole
    collection — and the output must be byte-for-byte identical to the
    brute-force path. Compare an indexed collection (optimized) against an
    unindexed one (full scan) over many random queries."""
    import random

    rng = random.Random(2024)
    docs = [
        {
            "_id": i,
            "loc": _pt(rng.uniform(-20, 20), rng.uniform(-20, 20)),
            "v": rng.randint(0, 5),
        }
        for i in range(400)
    ]
    indexed = client["geo_idx"]["gn_indexed"]
    scanned = client["geo_idx"]["gn_scanned"]
    indexed.insert_many(docs)
    scanned.insert_many(docs)
    indexed.create_index([("loc", "2dsphere")])  # only this one gets optimized

    for _ in range(40):
        cx, cy = rng.uniform(-20, 20), rng.uniform(-20, 20)
        max_d = rng.uniform(50_000, 2_000_000)  # metres
        stage = {
            "$geoNear": {
                "near": _pt(cx, cy),
                "distanceField": "d",
                "maxDistance": max_d,
                "key": "loc",
                "spherical": True,
            }
        }
        pipeline: list[dict] = [stage]
        if rng.random() < 0.4:
            pipeline.append({"$match": {"v": {"$gte": 2}}})
        if rng.random() < 0.3:
            stage["$geoNear"]["query"] = {"v": {"$lte": 4}}
        opt = [(d["_id"], round(d["d"], 6)) for d in indexed.aggregate(pipeline)]
        brute = [(d["_id"], round(d["d"], 6)) for d in scanned.aggregate(pipeline)]
        assert opt == brute, f"center={(cx, cy)} maxDistance={max_d}"
