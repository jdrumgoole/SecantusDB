"""Divergences found while giving the winning plan its work accounting
(RM-434, 2026-11-14), each measured against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest


def _cursor_stages(client: Any) -> list:
    client.drop_database("rm434_found")
    col = client.rm434_found.c
    col.insert_many([{"_id": i, "a": i % 10, "b": [1, 2]} for i in range(100)])
    col.create_index("a")
    e = col.database.command(
        "explain",
        {
            "aggregate": "c",
            "pipeline": [{"$match": {"a": {"$gt": 2}}}, {"$unwind": "$b"}, {"$limit": 3}],
            "cursor": {},
        },
        verbosity="executionStats",
    )
    stage = e["stages"][0]["$cursor"]["executionStats"]["executionStages"]
    out = []
    while stage:
        out.append({k: stage.get(k) for k in ("stage", "nReturned", "works", "saveState", "isEOF")})
        stage = stage.get("inputStage")
    return out


@pytest.mark.backlog("RM-692")
def test_a_cursor_the_pipeline_stops_reading_reports_one_batch(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's `$cursor` under `[$match, $unwind, $limit: 3]` loads
    one batch of 32 documents and stops: its FETCH and IXSCAN report
    `nReturned` 32, 32 works, `saveState` 2 and `isEOF` 0. This server's
    plan runs to the end (70 documents) and reports no work accounting."""
    want = _cursor_stages(mongod)
    assert want[0] == {"stage": "FETCH", "nReturned": 32, "works": 32, "saveState": 2, "isEOF": 0}
    assert _cursor_stages(rust_mongo) == want
