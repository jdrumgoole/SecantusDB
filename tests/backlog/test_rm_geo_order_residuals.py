"""Gap tests for the geo reads whose order is still not mongod's.

`tasks/backlog-rust-mongodb.md` 7.081: the order a read through a geo index
returns documents in is mongod's for a geo predicate the planner answers
from the geo index alone. These are the shapes left outside that, each
compared with the live `mongod` fixture (8.2.11), whose order is an index
scan this server does not replay for them.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

pymongo = pytest.importorskip("pymongo")


def _point(x: float, y: float) -> dict:
    return {"type": "Point", "coordinates": [x, y]}


SQUARE = {
    "$geometry": {"type": "Polygon", "coordinates": [[[0, 0], [3, 0], [3, 3], [0, 3], [0, 0]]]}
}

# The geo probe's `SPHERE` documents (tools/probes/geo_queries.py).
DOCS = [
    {"_id": 4, "loc": _point(1, 1), "k": "a"},
    {"_id": 1, "loc": _point(0, 0), "k": "b"},
    {"_id": 9, "loc": _point(10, 10), "k": "a"},
    {"_id": 2, "loc": _point(2.5, 2.5), "k": "b"},
    {"_id": 7, "loc": _point(-73.97, 40.77), "k": "a"},
    {"_id": 3, "loc": _point(-73.88, 40.78), "k": "b"},
    {"_id": 12, "loc": _point(179.9, 0), "k": "a"},
    {"_id": 5, "loc": _point(-179.9, 0), "k": "b"},
    {"_id": 8, "loc": _point(0, 89.9), "k": "a"},
    {"_id": 6, "loc": {"type": "LineString", "coordinates": [[0, 0], [2, 2]]}, "k": "b"},
    {"_id": 11, "loc": SQUARE["$geometry"], "k": "a"},
    {
        "_id": 10,
        "loc": {"type": "Polygon", "coordinates": [[[5, 5], [6, 5], [6, 6], [5, 6], [5, 5]]]},
        "k": "b",
    },
    {"_id": 13, "loc": [1.5, 1.5], "k": "a"},
    {"_id": 14, "loc": {"lng": 2, "lat": 2}, "k": "b"},
    {"_id": 15, "k": "a"},
    {"_id": 16, "loc": None, "k": "b"},
    {"_id": 17, "loc": {"type": "MultiPoint", "coordinates": [[1, 1], [30, 30]]}, "k": "a"},
    {"_id": 18, "loc": _point(3, 3), "k": "b"},
    {"_id": 19, "loc": _point(0, 3), "k": "a"},
    {"_id": 20, "loc": _point(1, 1), "k": "b"},
]


def _ids(client: Any, name: str, index: dict, filter_: dict) -> tuple:
    """The `_id`s a `find` returns, in order, over `DOCS` with one index."""
    client.drop_database(name)
    db = client[name]
    db.c.insert_many([dict(d) for d in DOCS])
    db.command("createIndexes", "c", indexes=[index])

    def run() -> list:
        reply = db.command({"find": "c", "filter": filter_})
        return [d["_id"] for d in reply["cursor"]["firstBatch"]]

    try:
        return mongo_answer(run)
    finally:
        client.drop_database(name)


GEO = {"key": {"loc": "2dsphere"}, "name": "g"}


@pytest.mark.backlog("RM-151")
def test_geo_predicates_in_or_branches(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers an `$or` of two geo predicates with one scan of
    the geo index per branch: 11, 1, 6, 4, 20, 13, 14, 2, 18, 19, then 9."""
    filter_ = {
        "$or": [
            {"loc": {"$geoWithin": SQUARE}},
            {"loc": {"$geoWithin": {"$centerSphere": [[10, 10], 0.01]}}},
        ]
    }
    assert _ids(rust_mongo, "rmgeo_or", GEO, filter_) == _ids(mongod, "rmgeo_or", GEO, filter_)


@pytest.mark.backlog("RM-151")
def test_geo_predicate_or_id(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: an `$or` of a geo predicate and an `_id` reads the
    `_id` index and the geo index, and returns 9 first, then the geo scan."""
    filter_ = {"$or": [{"loc": {"$geoWithin": SQUARE}}, {"_id": 9}]}
    assert _ids(rust_mongo, "rmgeo_orid", GEO, filter_) == _ids(mongod, "rmgeo_orid", GEO, filter_)


@pytest.mark.backlog("RM-151")
def test_compound_geo_index_with_a_range_on_its_trailing_field(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 bounds `{loc: "2dsphere", k: 1}`'s `k` by `$gte: "a"`
    and returns the scan's order: 11, 1, 6, 4, 20, 13, 14, 2, 18, 19."""
    index = {"key": {"loc": "2dsphere", "k": 1}, "name": "gk"}
    filter_ = {"loc": {"$geoWithin": SQUARE}, "k": {"$gte": "a"}}
    assert _ids(rust_mongo, "rmgeo_gk", index, filter_) == _ids(mongod, "rmgeo_gk", index, filter_)
