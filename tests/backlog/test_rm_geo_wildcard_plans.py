"""Gap tests found closing RM-777 (2026-11-24): `$geoWithin` / `$geoIntersects`
and wildcard plans, measured against mongod 8.2.11 on the same documents.
"""

from __future__ import annotations

from typing import Any

import pymongo
import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_storage_model import _plan_collection

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


@pytest.mark.backlog("RM-779")
def test_a_near_with_a_range_on_the_leading_field_returns_every_match(
    mongod: Any, rust_mongo: Any
) -> None:
    """`{pts: {$near: ...}, t: {$gte: 2}}` over `{t: 1, pts: "2dsphere"}`
    returns the 30 documents with `t` of 2 or 3 on mongod 8.2.11;
    `secantusd-mdb` returns 28 (two `t: 3` documents are missing). An
    equality on `t` and a range on the trailing field of a `2d` index return
    them all."""
    flt = {"pts": {"$near": {"$geometry": NEAR_PT}}, "t": {"$gte": 2}}
    ref = sorted(d["_id"] for d in _load(mongod).find(flt))
    got = sorted(d["_id"] for d in _load(rust_mongo).find(flt))
    assert got == ref


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
