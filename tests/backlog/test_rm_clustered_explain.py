"""A clustered collection's plans, as mongod 8.2.11 names them.

A find on `_id` of a collection made with `clusteredIndex` reads the table
itself (the RecordId is the `_id`): a point read is
`EXPRESS_CLUSTERED_IXSCAN`, a range a `CLUSTERED_IXSCAN` bounded by
`minRecord` / `maxRecord` (measured on mongod 8.2.11, 2026-11-21).
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

_SPEC = {"key": {"_id": 1}, "unique": True, "name": "c"}


def _stage(client: Any, flt: dict) -> dict:
    db = client[f"cl_{uuid.uuid4().hex[:12]}"]
    db.create_collection("c", clusteredIndex=_SPEC)
    db.c.insert_many([{"_id": i, "x": i} for i in range(1, 6)])
    plan = db.c.find(flt).explain()["queryPlanner"]["winningPlan"]
    return {k: plan.get(k) for k in ("stage", "direction", "minRecord", "maxRecord")}


@pytest.mark.backlog("RM-739")
def test_an_id_point_read_is_an_express_clustered_scan(rust_mongo: Any) -> None:
    assert _stage(rust_mongo, {"_id": 3})["stage"] == "EXPRESS_CLUSTERED_IXSCAN"


@pytest.mark.backlog("RM-739")
def test_an_id_range_is_a_bounded_clustered_scan(rust_mongo: Any) -> None:
    assert _stage(rust_mongo, {"_id": {"$gt": 1}}) == {
        "stage": "CLUSTERED_IXSCAN",
        "direction": "forward",
        "minRecord": 1,
        "maxRecord": None,
    }
