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


@pytest.mark.backlog_fixed("RM-783")
def test_a_near_over_a_compound_2dsphere_index_without_a_single_interval_is_refused(
    mongod: Any, rust_mongo: Any
) -> None:
    """Over `{t: 1, pts: "2dsphere"}` mongod 8.2.11 refuses (291,
    NoQueryExecutionPlans) a `$near` that no predicate bounds the LEADING
    field `t` of: "unable to find index for $geoNear query" when `t` has no
    usable predicate, "No query solutions" when only a negation, an `$or`
    or the like touches it; it answers `$exists`, `$in`, `$type`, a range
    and a hint. A compound `2d` index answers all of these."""
    queries = [
        {},
        {"z": 3},
        {"t": {"$ne": 2}},
        {"t": {"$nin": [1, 2]}},
        {"t": {"$not": {"$gt": 1}}},
        {"t": {"$exists": False}},
        {"t": {"$size": 1}},
        {"$or": [{"t": 1}, {"t": {"$gt": 5}}]},
        {"z": 3, "t": {"$ne": 2}},
        {"t": 2},
        {"t": {"$gt": 1}},
        {"t": {"$in": [1, 2]}},
        {"t": {"$in": []}},
        {"t": {"$exists": True}},
        {"t": {"$type": "int"}},
        {"t": {"$ne": 1, "$gt": 0}},
    ]
    near = {"pts": {"$near": {"$geometry": NEAR_PT}}}

    def read(coll: Any, q: dict) -> list:
        flt = {**near, **q}
        out: list = []
        for fn in (
            lambda: len(list(coll.find(flt))),
            lambda: len(list(coll.find(flt, {"z": 1}).sort("z", 1).limit(3).skip(1))),
            lambda: coll.database.command("count", coll.name, query=flt)["n"],
            lambda: coll.database.command(
                {"explain": {"find": coll.name, "filter": flt}, "verbosity": "queryPlanner"}
            )["queryPlanner"]["winningPlan"]["stage"],
            lambda: coll.database.command("distinct", coll.name, key="z", query=flt)["values"],
            lambda: coll.update_many(flt, {"$set": {"u": 1}}).matched_count,
            lambda: len(
                list(
                    coll.aggregate(
                        [
                            {
                                "$geoNear": {
                                    "near": NEAR_PT,
                                    "key": "pts",
                                    "spherical": True,
                                    "distanceField": "d",
                                    "query": q,
                                }
                            }
                        ]
                    )
                )
            ),
            lambda: len(list(coll.find(flt).hint("t_pts"))),
        ):
            got = mongo_answer(fn)
            out.append(
                (got[0], got[1] if got[0] == "err" else None, got[2:] if got[0] == "err" else None)
            )
        return out

    answers = []
    for client in (mongod, rust_mongo):
        coll = _load(client)
        answers.append([read(coll, q) for q in queries])
    assert answers[1] == answers[0]
    # A `2d` index answers each of them.
    for client in (mongod, rust_mongo):
        coll = _load(client)
        for q in queries:
            got = mongo_answer(
                lambda q=q, coll=coll: list(coll.find({"flat": {"$near": [3, 4]}, **q}))
            )
            assert got[0] == "ok", (q, got)


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


@pytest.mark.backlog_fixed("RM-781")
def test_a_hint_to_a_wildcard_index_no_predicate_can_use_is_refused(
    mongod: Any, rust_mongo: Any
) -> None:
    """mongod 8.2.11 refuses (NoQueryExecutionPlans, 291) a hint to a
    wildcard index when no predicate of the query bounds a path it keys,
    since it will not build a whole-index scan of one: "$hint: refusing to
    build whole-index solution, because it's a wildcard index" when the
    query names one path the index expands to, "No query solutions" when it
    names none or several. `{"w.a": {$ne: 3}}` and a `$geoWithin` hinted to
    `$**` are the first. A query that bounds a path is planned."""
    queries = [
        {"w.a": {"$ne": 3}},
        {"poly": {"$geoWithin": {"$geometry": NEAR_PT_POLY}}},
        {},
        {"_id": 5},
        {"z": {"$ne": 3}, "w.a": {"$ne": 3}},
        {"$or": [{"w.a": {"$ne": 1}}, {"z": 2}]},
        {"$or": [{"w.a": 1}, {"z": 2}]},
        {"w.a": {"$size": 1}},
        {"w.a": None},
        {"w.a": 3},
        {"w.a": {"$gt": 1, "$ne": 2}},
        {"z": {"$mod": [2, 0]}},
        {"w.a": {"$in": []}},
        {"$expr": {"$eq": ["$z", 3]}},
    ]
    hints = ["all_w", "w_w", "proj_w", "excl_w", "part_w"]

    def read(coll: Any, flt: dict, hint: str) -> list:
        out: list = []
        for fn in (
            lambda: len(list(coll.find(flt).hint(hint))),
            lambda: len(list(coll.find(flt, {"z": 1}).hint(hint).sort("z", 1).limit(3).skip(1))),
            lambda: coll.database.command("count", coll.name, query=flt, hint=hint)["n"],
            lambda: coll.database.command(
                {
                    "explain": {"find": coll.name, "filter": flt, "hint": hint},
                    "verbosity": "queryPlanner",
                }
            )["queryPlanner"]["winningPlan"]["stage"],
            lambda: len(list(coll.aggregate([{"$match": flt}], hint=hint))),
            lambda: coll.database.command(
                "update", coll.name, updates=[{"q": flt, "u": {"$set": {"u": 1}}, "hint": hint}]
            ).get("writeErrors"),
        ):
            got = mongo_answer(fn)
            out.append(got if got[0] == "err" else got[:1] + (None,))
        return out

    answers = []
    for client in (mongod, rust_mongo):
        coll = _load(client)
        answers.append([[read(coll, q, h) for h in hints] for q in queries])
    assert answers[1] == answers[0]


@pytest.mark.backlog("RM-785")
def test_a_hinted_near_over_an_unbounded_leading_field_scans_the_whole_index(
    mongod: Any, rust_mongo: Any
) -> None:
    """Hinted to `{t: 1, pts: "2dsphere"}` with no usable predicate on `t`,
    mongod 8.2.11 plans a FETCH over a whole-index IXSCAN (bounds
    `[MinKey, MaxKey]` on both fields) with the `$near` as the fetch's
    filter, and returns the documents in the index's key order; for `{z: 3}`
    `[12, 48, 21, 57, 30, 3, 39]`. `secantusd-mdb` returns them nearest
    first, and plans GEO_NEAR_2DSPHERE for `t: {$ne: 2}`."""
    near = {"pts": {"$near": {"$geometry": NEAR_PT}}}
    answers = []
    for client in (mongod, rust_mongo):
        coll = _load(client)
        one = []
        for q in ({"z": 3}, {"t": {"$ne": 2}}, {}):
            ids = [d["_id"] for d in coll.find({**near, **q}).hint("t_pts")]
            stage = _winning(coll, {**near, **q}, hint="t_pts")["queryPlanner"]["winningPlan"]
            one.append((ids, stage["stage"]))
        answers.append(one)
    assert answers[1] == answers[0]


@pytest.mark.backlog("RM-786")
def test_a_hinted_geo_near_stage_over_an_unbounded_leading_field_fails_in_the_executor(
    mongod: Any, rust_mongo: Any
) -> None:
    """An `aggregate` `$geoNear` hinted to `t_pts` with a `query` that bounds
    no leading field (`{z: 3}`, `{t: {$ne: 2}}`) fails on mongod 8.2.11 with
    9911902, "Query returned a document that is unexpectedly missing the
    geoNear distance: {...}": the hint's whole-index scan returns documents
    the stage never measured. `secantusd-mdb` answers the documents."""
    answers = []
    for client in (mongod, rust_mongo):
        coll = _load(client)
        one = []
        for q in ({"z": 3}, {"t": {"$ne": 2}}, {"t": 2}):
            got = mongo_answer(
                lambda q=q, coll=coll: list(
                    coll.aggregate(
                        [
                            {
                                "$geoNear": {
                                    "near": NEAR_PT,
                                    "key": "pts",
                                    "spherical": True,
                                    "distanceField": "d",
                                    "query": q,
                                }
                            }
                        ],
                        hint="t_pts",
                    )
                )
            )
            one.append((got[0], got[1] if got[0] == "err" else None))
        answers.append(one)
    assert answers[1] == answers[0]


@pytest.mark.backlog("RM-787")
def test_an_explain_of_a_geo_near_stage_is_the_geo_near_cursor_with_its_query(
    mongod: Any, rust_mongo: Any
) -> None:
    """mongod 8.2.11's `explain` of `aggregate` with a leading `$geoNear`
    over `{t: 1, pts: "2dsphere"}` lists a `$geoNearCursor` stage whose
    `parsedQuery` is the `query` and the near leaf
    (`{$and: [{t: {$eq: 2}}, {pts: {$nearSphere: ...}}]}`) over a
    GEO_NEAR_2DSPHERE winning plan; `secantusd-mdb` lists a `$cursor` with
    an empty `parsedQuery`."""
    answers = []
    for client in (mongod, rust_mongo):
        coll = _load(client)
        reply = coll.database.command(
            {
                "explain": {
                    "aggregate": coll.name,
                    "pipeline": [
                        {
                            "$geoNear": {
                                "near": NEAR_PT,
                                "key": "pts",
                                "spherical": True,
                                "distanceField": "d",
                                "query": {"t": 2},
                            }
                        }
                    ],
                    "cursor": {},
                },
                "verbosity": "queryPlanner",
            }
        )
        stage = reply["stages"][0]
        name = next(iter(stage))
        planner = stage[name]["queryPlanner"]
        answers.append((name, planner["parsedQuery"], planner["winningPlan"]["stage"]))
    assert answers[1] == answers[0]


NEAR_PT_POLY = {
    "type": "Polygon",
    "coordinates": [[[0, 0], [19, 0], [19, 19], [0, 19], [0, 0]]],
}

assert pymongo  # the fixtures need it
