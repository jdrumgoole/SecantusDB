"""Gap tests found closing RM-777 (2026-11-24): `$geoWithin` / `$geoIntersects`
and wildcard plans, measured against mongod 8.2.11 on the same documents.
"""

from __future__ import annotations

from typing import Any

import pymongo
import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_storage_model import _geo_wildcard_docs, _plan_collection

NEAR_PT = {"type": "Point", "coordinates": [5, 5]}


def _load(client: Any) -> Any:
    coll = client["gw777"]["p"]
    coll.drop()
    _plan_collection(coll)
    return coll


def _winning(coll: Any, flt: dict, verbosity: str = "queryPlanner", **kw: Any) -> dict:
    reply = coll.database.command(
        {"explain": {"find": coll.name, "filter": flt, **kw}, "verbosity": verbosity}
    )
    return reply


@pytest.mark.backlog_fixed("RM-779")
def test_a_near_with_a_range_on_the_leading_field_returns_every_match(
    mongod: Any, rust_mongo: Any
) -> None:
    """`{pts: {$near: ...}, t: {$gte: 2}}` over `{t: 1, pts: "2dsphere"}`
    returns the 30 documents with `t` of 2 or 3 on mongod 8.2.11;
    `secantusd-mdb` returned 28 (two `t: 3` documents were missing): the
    near search's scan left `t: 2` in the last interval of cells and began
    `t: 3` there, past the cells of the first. An equality on `t` and a
    range on the trailing field of a `2d` index returned them all. The
    same for each range, a `$geoNear` and a `$maxDistance`."""
    near = {"$geometry": NEAR_PT}
    reads = {
        "$gte 2": lambda c: c.find({"pts": {"$near": near}, "t": {"$gte": 2}}),
        "$gt 1": lambda c: c.find({"pts": {"$near": near}, "t": {"$gt": 1}}),
        "$lte 1": lambda c: c.find({"pts": {"$near": near}, "t": {"$lte": 1}}),
        "$gt 0 $lt 3": lambda c: c.find({"pts": {"$near": near}, "t": {"$gt": 0, "$lt": 3}}),
        "$nearSphere": lambda c: c.find({"pts": {"$nearSphere": near}, "t": {"$gte": 2}}),
        "$maxDistance": lambda c: c.find(
            {"pts": {"$near": {"$geometry": NEAR_PT, "$maxDistance": 5_000_000}}, "t": {"$gte": 2}}
        ),
        "$geoNear": lambda c: c.aggregate(
            [
                {
                    "$geoNear": {
                        "near": NEAR_PT,
                        "key": "pts",
                        "spherical": True,
                        "distanceField": "d",
                        "query": {"t": {"$gte": 2}},
                    }
                }
            ]
        ),
    }
    ref, got = _load(mongod), _load(rust_mongo)
    for label, read in reads.items():
        assert sorted(d["_id"] for d in read(got)) == sorted(d["_id"] for d in read(ref)), label


@pytest.mark.backlog_fixed("RM-779")
def test_a_near_over_a_multikey_trailing_field_bounds_it_by_one_predicate(
    mongod: Any, rust_mongo: Any
) -> None:
    """Two predicates on a multikey field may be met by different elements
    of an array, so mongod bounds it by ONE (`t: {$gte: 1, $lte: 2}` scans
    `[-inf, 2]`, and a document holding `[3, 0]` is returned). The near
    search intersected them, and a `2d` or `2dsphere` `$near` and a
    `$geoNear` returned 45 of 60 documents (0 of 30 for `$gt: 0, $lt: 1`)."""

    def arrays(coll: Any) -> Any:
        coll.drop()
        docs = _geo_wildcard_docs()
        for d in docs:
            d["t"] = [d["t"], (d["t"] + 1) % 4]
        coll.insert_many(docs)
        coll.create_index([("flat", "2d"), ("t", 1)], name="flat_t")
        coll.create_index([("t", 1), ("pts", "2dsphere")], name="t_pts")
        return coll

    sphere = {"$geometry": NEAR_PT}
    reads = {}
    for label, rng in {
        "$gte $lte": {"$gte": 1, "$lte": 2},
        "$gt $lt": {"$gt": 0, "$lt": 1},
        "$gte $lt": {"$gte": 3, "$lt": 1},
    }.items():
        reads[f"2dsphere {label}"] = lambda c, rng=rng: c.find({"pts": {"$near": sphere}, "t": rng})
        reads[f"2d {label}"] = lambda c, rng=rng: c.find({"flat": {"$near": [3, 4]}, "t": rng})
        reads[f"$geoNear {label}"] = lambda c, rng=rng: c.aggregate(
            [
                {
                    "$geoNear": {
                        "near": [3, 4],
                        "key": "flat",
                        "distanceField": "d",
                        "query": {"t": rng},
                    }
                }
            ]
        )
    ref = arrays(mongod["gw779"]["m"])
    got = arrays(rust_mongo["gw779"]["m"])
    for label, read in reads.items():
        assert sorted(d["_id"] for d in read(got)) == sorted(d["_id"] for d in read(ref)), label


@pytest.mark.backlog("RM-783")
def test_a_near_over_a_compound_2dsphere_index_without_a_single_interval_is_refused(
    mongod: Any, rust_mongo: Any
) -> None:
    """Over `{t: 1, pts: "2dsphere"}` mongod 8.2.11 refuses (291,
    NoQueryExecutionPlans, "unable to find index for $geoNear query") a
    `$near` whose leading field `t` has no predicate, or one of `$ne`,
    `$nin` or `$not` (bounds of two intervals); it answers `$exists`, `$in`,
    `$type` and a range. `secantusd-mdb` answers every one, as it does for a
    `2d` index, which mongod answers too."""
    queries = [
        {},
        {"z": 3},
        {"t": {"$ne": 2}},
        {"t": {"$nin": [1, 2]}},
        {"t": {"$not": {"$gt": 1}}},
    ]
    answers = []
    for client in (mongod, rust_mongo):
        coll = _load(client)
        one = []
        for q in queries:
            got = mongo_answer(
                lambda q=q, coll=coll: list(
                    coll.find({"pts": {"$near": {"$geometry": NEAR_PT}}, **q})
                )
            )
            one.append((got[0], got[1] if got[0] == "err" else None))
        answers.append(one)
    assert answers[1] == answers[0]


@pytest.mark.backlog("RM-780")
def test_a_near_stage_reports_its_inner_scans_and_counts_their_keys(
    mongod: Any, rust_mongo: Any
) -> None:
    """mongod's `executionStats` for `{flat: {$near: [3, 4]}, t: 1}` show the
    GEO_NEAR_2D stage's `inputStages` (one FETCH over IXSCAN per ring it
    searched, the equality on `t` as the scan's filter) and 87 keys examined;
    `secantusd-mdb` shows no `inputStages` and 79 keys."""
    flt = {"flat": {"$near": [3, 4]}, "t": 1}
    answers = []
    for client in (mongod, rust_mongo):
        reply = _winning(_load(client), flt, "executionStats")
        plan = reply["queryPlanner"]["winningPlan"]
        answers.append(
            (
                plan["stage"],
                len(plan.get("inputStages", [])),
                reply["executionStats"]["totalKeysExamined"],
            )
        )
    assert answers[1] == answers[0]


@pytest.mark.backlog("RM-781")
def test_a_hint_to_a_wildcard_index_no_predicate_can_use_is_refused(
    mongod: Any, rust_mongo: Any
) -> None:
    """mongod 8.2.11 refuses (NoQueryExecutionPlans, 291) a hint to a wildcard
    index when no predicate of the query bounds a path it keys, since it
    will not build a whole-index scan of one: `{"w.a": {$ne: 3}}` and a
    `$geoWithin` hinted to `$**`. `secantusd-mdb` plans a scan of the whole
    `$**` index."""
    queries = [
        {"w.a": {"$ne": 3}},
        {"poly": {"$geoWithin": {"$geometry": NEAR_PT_POLY}}},
    ]
    answers = []
    for client in (mongod, rust_mongo):
        coll = _load(client)
        one = []
        for flt in queries:
            got = mongo_answer(lambda flt=flt, coll=coll: list(coll.find(flt).hint("all_w")))
            one.append((got[0], got[1] if got[0] == "err" else None))
        answers.append(one)
    assert answers[1] == answers[0]


NEAR_PT_POLY = {
    "type": "Polygon",
    "coordinates": [[[0, 0], [19, 0], [19, 19], [0, 19], [0, 0]]],
}

assert pymongo  # the fixtures need it
