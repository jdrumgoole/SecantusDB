"""Gap tests found while fixing RM-707 (a GEO_NEAR stage's executionStats)."""

import uuid
from typing import Any

import pytest


def _explain(client: Any, docs: list, index: list, find: dict) -> dict:
    db = client["rm_found_707_" + uuid.uuid4().hex[:8]]
    db.c.insert_many(docs)
    db.c.create_index(index)
    try:
        return db.command("explain", {"find": "c", **find}, verbosity="executionStats")
    finally:
        client.drop_database(db.name)


@pytest.mark.backlog("RM-712")
def test_a_multipoint_2dsphere_near_ring_scan_is_multikey(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 marks a 2dsphere index multikey once a MultiPoint gives
    a document several keys: each GEO_NEAR_2DSPHERE ring's IXSCAN reports
    `isMultiKey: true` and `multiKeyPaths: {loc: ["loc"]}`. This server's
    catalog does not, so its ring scans report `false` and `[]`."""
    docs = [
        {"_id": n, "loc": {"type": "MultiPoint", "coordinates": [[n, n], [n + 0.5, 0]]}}
        for n in range(6)
    ]
    find = {"filter": {"loc": {"$near": {"$geometry": {"type": "Point", "coordinates": [0, 0]}}}}}

    def scan(client: Any) -> tuple:
        stage = _explain(client, docs, [("loc", "2dsphere")], find)["executionStats"][
            "executionStages"
        ]
        ixscan = stage["inputStages"][0]["inputStage"]
        return ixscan["isMultiKey"], ixscan["multiKeyPaths"]

    assert scan(mongod) == (True, {"loc": ["loc"]})
    assert scan(rust_mongo) == scan(mongod)


@pytest.mark.backlog("RM-713")
def test_a_near_over_a_compound_2d_index_with_its_trailing_field_plans_geo_near(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 answers `{loc: {$near: [0, 0]}, z: 1}` over `{loc: "2d",
    z: 1}` with GEO_NEAR_2D, the `z` predicate a key filter on each ring's
    IXSCAN. This server's plan is a FETCH over another stage."""
    docs = [{"_id": n, "z": n % 2, "loc": [n, n]} for n in range(8)]
    find = {"filter": {"loc": {"$near": [0, 0]}, "z": 1}}

    def top(client: Any) -> tuple:
        stats = _explain(client, docs, [("loc", "2d"), ("z", 1)], find)["executionStats"]
        return stats["executionStages"]["stage"], stats["totalDocsExamined"]

    assert top(mongod) == ("GEO_NEAR_2D", 4)
    assert top(rust_mongo) == top(mongod)


@pytest.mark.backlog("RM-714")
def test_a_limit_over_a_geo_near_reports_its_works(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's LIMIT over GEO_NEAR_2D (`find` with `limit: 3`)
    reports its work accounting: one work more than the stage below it.
    This server's LIMIT prints no `works`, `advanced` or `needTime`."""
    docs = [{"_id": n, "loc": [n, n]} for n in range(12)]
    find = {"filter": {"loc": {"$near": [0, 0]}}, "limit": 3}

    def limit(client: Any) -> tuple:
        stage = _explain(client, docs, [("loc", "2d")], find)["executionStats"]["executionStages"]
        below = stage["inputStage"]
        return (
            stage["stage"],
            stage.get("works"),
            stage.get("advanced"),
            stage.get("needTime"),
            below.get("works"),
        )

    want = limit(mongod)
    assert want[0] == "LIMIT" and want[1] == want[4] + 1
    assert limit(rust_mongo) == want
