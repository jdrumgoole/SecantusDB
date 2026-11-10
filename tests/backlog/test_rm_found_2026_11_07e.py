"""A divergence found while moving the `$near` tie order to the Linux build
(RM-231, 2026-11-07): this server's S2 arithmetic is the macOS build's.

mongod's builds disagree on S2's floating point: the Linux build rounds each
step, the macOS arm64 build fuses multiply-adds. The Linux build is the
exemplar (Joe, 2026-10-21). The reference here is mongod 8.2.12 on Linux,
x86-64 and arm64 alike, run in containers (`mongo:8.2`) -- not the macOS
mongod on PATH, which answers the other way -- so its answers are literals.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

pymongo = pytest.importorskip("pymongo")


@pytest.mark.backlog_fixed("RM-393")
def test_a_line_through_the_centre_is_at_distance_zero(rust_mongo: Any) -> None:
    """A 2dsphere `$geoNear` from a LineString's own first vertex: Linux
    mongod 8.2.12 measures 0.0. macOS 8.2.1 measures 6.4256766055857336e-12,
    and this server answered 6.425676607797001e-12, a floor copied from that
    build, so in a `$near` the line came after a point and a polygon at
    distance 0 instead of before them. Its S2 arithmetic now rounds each
    step, as the Linux build does, and `$geoNear` measures with it."""
    db = rust_mongo["rm393"]
    db.c.drop()
    line = {"type": "LineString", "coordinates": [[0.0, 0.0], [2.0, 2.0]]}
    db.c.insert_one({"_id": 6, "loc": line})
    db.c.create_index([("loc", "2dsphere")])
    stage = {
        "$geoNear": {"near": {"type": "Point", "coordinates": [0.0, 0.0]}, "distanceField": "d"}
    }
    got = mongo_answer(lambda: [d["d"] for d in db.c.aggregate([stage])])
    assert got == ("ok", [0.0])
