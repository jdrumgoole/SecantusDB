"""Gap tests found while fixing RM-578 (a `$near` read's GEO_NEAR plan)."""

import uuid
from typing import Any

import pytest


def _seed_points(db: Any) -> None:
    db.c.insert_many([{"_id": n, "z": n % 2, "loc": [n, n]} for n in range(12)])
    db.c.create_index([("loc", "2d")])


def _execution_stats(client: Any) -> dict:
    db = client["rm_found_578_" + uuid.uuid4().hex[:8]]
    _seed_points(db)
    try:
        stats = db.command(
            "explain",
            {"find": "c", "filter": {"loc": {"$near": [0, 0]}}},
            verbosity="executionStats",
        )["executionStats"]
    finally:
        client.drop_database(db.name)
    stage = stats["executionStages"]
    return {
        "stage": stage["stage"],
        "fields": sorted(k for k in stage if k != "executionTimeMillisEstimate"),
        "nReturned": stats["nReturned"],
        "totalKeysExamined": stats["totalKeysExamined"],
        "totalDocsExamined": stats["totalDocsExamined"],
    }


@pytest.mark.backlog("RM-707")
def test_a_geo_near_stage_reports_mongods_execution_stats(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's GEO_NEAR_2D reports `searchIntervals`, the rings'
    `inputStages`, its work counts and 52 keys examined for twelve points.
    This server's stage reports none of them and counts no keys."""
    answers = {
        server: _execution_stats(client)
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"]["stage"] == "GEO_NEAR_2D"
    assert "searchIntervals" in answers["mongod"]["fields"]
    assert answers["rust"] == answers["mongod"]
