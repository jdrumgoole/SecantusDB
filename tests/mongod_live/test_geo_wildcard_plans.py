"""`$geoWithin` / `$geoIntersects` / wildcard plans against mongod 8.2 (RM-777).

mongod reads a `2d` / `2dsphere` index for a `$geoWithin` / `$geoIntersects`
(the covering as the bounds, the exact test a FETCH's filter) and a wildcard
index for a predicate on a path it keys, as one IXSCAN per path keyed
`{$_path: 1, <path>: 1}`, multikey where the index holds a metadata key for the
path. A compound `$near` takes the equalities on the index's other fields as
the near stage's bounds. Each case compares the winning plan's stages with
their bounds and flags, the number of rejected plans, and what the scan
examined; a `$near` is compared at its root (its inner scans are not shown).
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

SQUARE = {"type": "Polygon", "coordinates": [[[0, 0], [9, 0], [9, 9], [0, 9], [0, 0]]]}
BIG = {"type": "Polygon", "coordinates": [[[0, 0], [19, 0], [19, 19], [0, 19], [0, 0]]]}
POINT = {"type": "Point", "coordinates": [3, 3]}
NEAR_PT = {"type": "Point", "coordinates": [5, 5]}

INDEXES: list[tuple[list, dict]] = [
    ([("flat", "2d"), ("t", 1)], {"name": "flat_t"}),
    ([("t", 1), ("pts", "2dsphere")], {"name": "t_pts"}),
    ([("poly", "2dsphere")], {"name": "poly_s"}),
    ([("$**", 1)], {"name": "all_w"}),
]

#: name -> (filter, hint, whether to compare the scan's counters)
CASES: dict[str, tuple[dict, str | None, bool]] = {
    "2d $geoWithin $box": ({"flat": {"$geoWithin": {"$box": [[-5, -5], [8, 9]]}}}, None, True),
    "2d $geoWithin $polygon": (
        {"flat": {"$geoWithin": {"$polygon": [[0, 0], [9, 0], [9, 9]]}}},
        None,
        True,
    ),
    "2d $geoWithin $geometry is not served": (
        {"flat": {"$geoWithin": {"$geometry": SQUARE}}},
        None,
        True,
    ),
    "2d $geoIntersects is not served": (
        {"flat": {"$geoIntersects": {"$geometry": SQUARE}}},
        None,
        True,
    ),
    "2dsphere $geoIntersects polygon": (
        {"poly": {"$geoIntersects": {"$geometry": SQUARE}}},
        None,
        True,
    ),
    "2dsphere $geoIntersects point, hinted": (
        {"poly": {"$geoIntersects": {"$geometry": POINT}}},
        "poly_s",
        True,
    ),
    "2dsphere $geoWithin, a prefix equality": (
        {"pts": {"$geoWithin": {"$geometry": BIG}}, "t": 2},
        "t_pts",
        True,
    ),
    "2dsphere $geoWithin, no prefix predicate": (
        {"pts": {"$geoWithin": {"$geometry": BIG}}},
        None,
        True,
    ),
    "$near with an equality on the trailing field": (
        {"flat": {"$near": [3, 4]}, "t": 1},
        None,
        False,
    ),
    "$near with a range on the trailing field": (
        {"flat": {"$near": [3, 4]}, "t": {"$gte": 1}},
        None,
        False,
    ),
    "$near with the equality first": (
        {"pts": {"$near": {"$geometry": NEAR_PT}}, "t": 3},
        None,
        False,
    ),
    "$near with an equality and a clause off the index": (
        {"flat": {"$near": [3, 4]}, "z": 1, "t": 1},
        None,
        False,
    ),
    "wildcard equality": ({"w.a": 3}, None, True),
    "wildcard range": ({"w.c.d": {"$gt": 2, "$lte": 5}}, None, True),
    "wildcard $in": ({"w.a": {"$in": [3, 4]}}, None, True),
    "wildcard under a hint": ({"w.a": 3}, "all_w", True),
    "wildcard on a multikey path": ({"w.b": 2}, None, True),
    "wildcard on a path below an array": ({"arr.k": 3}, None, True),
    "wildcard $elemMatch": ({"arr": {"$elemMatch": {"k": 3}}}, None, True),
    "wildcard $exists": ({"w.a": {"$exists": True}}, None, True),
    "wildcard $type": ({"w.a": {"$type": "int"}}, None, True),
    "wildcard $regex": ({"w.b": {"$regex": "^s"}}, None, True),
    "wildcard on two paths": ({"w.a": 3, "z": 4}, None, True),
    "wildcard on two paths, a tie": ({"w.a": {"$gt": 1}, "w.c.d": {"$lt": 4}}, None, True),
    "wildcard $ne is not served": ({"w.a": {"$ne": 3}}, None, True),
    "wildcard $exists false is not served": ({"w.a": {"$exists": False}}, None, True),
    "wildcard equality to null is not served": ({"w.c.e": None}, None, True),
    "wildcard equality to an object is not served": ({"w.c": {"d": 1, "e": None}}, None, True),
    "wildcard equality to an empty object": ({"w.c": {}}, None, True),
    "wildcard $or over two paths": ({"$or": [{"w.a": 3}, {"z": 4}]}, None, True),
}


def _docs() -> list[dict]:
    docs = []
    for i in range(60):
        x = (i * 7) % 40 - 20 + ((i * 37) % 101) / 1009
        y = (i * 11) % 30 - 15 + ((i * 53) % 103) / 1013
        docs.append(
            {
                "_id": i,
                "t": i % 4,
                "flat": [x, y],
                "pts": {"type": "MultiPoint", "coordinates": [[x, y], [y / 2, x / 2]]},
                "poly": {
                    "type": "Polygon",
                    "coordinates": [[[x, y], [x + 2, y], [x + 2, y + 2], [x, y + 2], [x, y]]],
                },
                "w": {"a": i % 5, "b": [i % 3, f"s{i % 4}"], "c": {"d": i % 7, "e": None}},
                "z": i % 9,
                "arr": [{"k": i % 6}, {"k": (i + 1) % 6}],
            }
        )
    return docs


def _stage(plan: dict) -> dict:
    """The stage with what is compared of it, down its input."""
    keep = (
        "stage",
        "keyPattern",
        "indexName",
        "isMultiKey",
        "multiKeyPaths",
        "isSparse",
        "isPartial",
        "direction",
        "indexBounds",
        "filter",
    )
    out = {k: plan[k] for k in keep if k in plan}
    if "inputStage" in plan:
        out["inputStage"] = _stage(plan["inputStage"])
    return out


def _answer(client: Any, db_name: str, case: str) -> dict:
    flt, hint, counters = CASES[case]
    db = client[db_name]
    command: dict = {"find": "p", "filter": flt}
    if hint:
        command["hint"] = hint
    reply = db.command({"explain": command, "verbosity": "executionStats"})
    plan = reply["queryPlanner"]["winningPlan"]
    out: dict = {
        "plan": _stage(plan),
        "rejected": len(reply["queryPlanner"]["rejectedPlans"]),
        "ids": sorted(d["_id"] for d in db.p.find(flt, hint=hint)),
    }
    if counters:
        stats = reply["executionStats"]
        out["examined"] = (
            stats["totalKeysExamined"],
            stats["totalDocsExamined"],
            stats["nReturned"],
        )
    return out


def _load(client: Any) -> str:
    name = "geowc_" + uuid.uuid4().hex[:8]
    coll = client[name]["p"]
    coll.insert_many(_docs())
    for spec, options in INDEXES:
        coll.create_index(spec, **options)
    return name


@pytest.mark.parametrize("case", list(CASES))
def test_geo_and_wildcard_plans_are_mongods(rust_mongo: Any, mongod: Any, case: str) -> None:
    ref_db = _load(mongod)
    ours_db = _load(rust_mongo)
    try:
        expected = _answer(mongod, ref_db, case)
        got = _answer(rust_mongo, ours_db, case)
    finally:
        mongod.drop_database(ref_db)
        rust_mongo.drop_database(ours_db)
    assert got == expected
