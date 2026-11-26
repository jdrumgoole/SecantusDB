"""A clustered collection's plans, as mongod 8.2.11 names them.

A find on `_id` of a collection made with `clusteredIndex` reads the table
itself (the RecordId is the `_id`): a point read is
`EXPRESS_CLUSTERED_IXSCAN`, a range a `CLUSTERED_IXSCAN` bounded by
`minRecord` / `maxRecord` (measured on mongod 8.2.11, 2026-11-21). An open
end is the type bracket's end, not null: `{_id: {$gt: 1}}` has
`maxRecord: Infinity` (re-measured on mongod 8.2.11, 2026-11-26).
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


@pytest.mark.backlog_fixed("RM-739")
def test_an_id_point_read_is_an_express_clustered_scan(rust_mongo: Any) -> None:
    assert _stage(rust_mongo, {"_id": 3})["stage"] == "EXPRESS_CLUSTERED_IXSCAN"


@pytest.mark.backlog_fixed("RM-739")
def test_an_id_range_is_a_bounded_clustered_scan(rust_mongo: Any) -> None:
    assert _stage(rust_mongo, {"_id": {"$gt": 1}}) == {
        "stage": "CLUSTERED_IXSCAN",
        "direction": "forward",
        "minRecord": 1,
        "maxRecord": float("inf"),
    }


def _collection(client: Any, ids: Any = None) -> Any:
    db = client[f"cl_{uuid.uuid4().hex[:12]}"]
    db.create_collection("c", clusteredIndex=_SPEC)
    db.c.insert_many([{"_id": i, "x": i} for i in (range(1, 6) if ids is None else ids)])
    return db


def _leaf(stage: dict) -> dict:
    while "inputStage" in stage:
        stage = stage["inputStage"]
    return stage


# The plans of the other commands over an `_id` range, and what RM-739 left.
#
# mongod 8.2.11, 2026-11-26, five `_id`s 1..5: `count` of `{_id: {$gt: 1}}`
# is COUNT <- CLUSTERED_IXSCAN (minRecord 1, maxRecord Infinity), an `update`
# of the range UPDATE <- CLUSTERED_IXSCAN, a `delete` BATCHED_DELETE <-
# CLUSTERED_IXSCAN and a `distinct` over it a CLUSTERED_IXSCAN; `{_id: 2}`
# counts as COUNT <- CLUSTERED_IXSCAN too.


@pytest.mark.backlog("RM-816")
def test_a_count_over_an_id_range_reads_the_clustered_scan(rust_mongo: Any) -> None:
    db = _collection(rust_mongo)
    plan = db.command(
        {"explain": {"count": "c", "query": {"_id": {"$gt": 1}}}, "verbosity": "queryPlanner"}
    )["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "COUNT"
    assert _leaf(plan)["stage"] == "CLUSTERED_IXSCAN"


@pytest.mark.backlog("RM-816")
def test_a_distinct_over_an_id_range_reads_the_clustered_scan(rust_mongo: Any) -> None:
    db = _collection(rust_mongo)
    plan = db.command(
        {
            "explain": {"distinct": "c", "key": "x", "query": {"_id": {"$gt": 1}}},
            "verbosity": "queryPlanner",
        }
    )["queryPlanner"]["winningPlan"]
    assert _leaf(plan)["stage"] == "CLUSTERED_IXSCAN"


@pytest.mark.backlog("RM-816")
def test_an_update_over_an_id_range_reads_the_clustered_scan(rust_mongo: Any) -> None:
    db = _collection(rust_mongo)
    update = {"q": {"_id": {"$gt": 1}}, "u": {"$set": {"x": 0}}, "multi": True}
    plan = db.command(
        {"explain": {"update": "c", "updates": [update]}, "verbosity": "queryPlanner"}
    )["queryPlanner"]["winningPlan"]
    assert plan["stage"] == "UPDATE"
    assert _leaf(plan)["stage"] == "CLUSTERED_IXSCAN"


@pytest.mark.backlog("RM-816")
def test_a_delete_over_an_id_range_reads_the_clustered_scan(rust_mongo: Any) -> None:
    db = _collection(rust_mongo)
    delete = {"q": {"_id": {"$gt": 1}}, "limit": 0}
    plan = db.command(
        {"explain": {"delete": "c", "deletes": [delete]}, "verbosity": "queryPlanner"}
    )["queryPlanner"]["winningPlan"]
    assert _leaf(plan)["stage"] == "CLUSTERED_IXSCAN"


# `$or` of two `_id` ranges (mongod 8.2.11, 2026-11-26): SUBPLAN <- OR <-
# two CLUSTERED_IXSCANs, `[NaN, 2)` and `(8, Infinity]` for `$lt: 2` and
# `$gt: 8`; this server reads it as one COLLSCAN under the SUBPLAN with the
# whole `$or` as its filter, because it plans no branch of an `$or` over the
# clustered key.


@pytest.mark.backlog("RM-817")
def test_an_or_of_id_ranges_is_an_or_of_clustered_scans(rust_mongo: Any) -> None:
    db = _collection(rust_mongo, range(1, 11))
    plan = db.c.find({"$or": [{"_id": {"$gt": 8}}, {"_id": {"$lt": 2}}]}).explain()
    win = plan["queryPlanner"]["winningPlan"]
    assert win["stage"] == "SUBPLAN"
    assert win["inputStage"]["stage"] == "OR"
    scans = win["inputStage"]["inputStages"]
    assert [(s["stage"], s["direction"], s["maxRecord"]) for s in scans] == [
        ("CLUSTERED_IXSCAN", "forward", 2),
        ("CLUSTERED_IXSCAN", "forward", float("inf")),
    ]


# `min` / `max` with a hint of the clustered key are the scan's own ends
# (mongod 8.2.11, 2026-11-26): `min {_id: 1}`, `max {_id: 3}` is
# `minRecord: 1, maxRecord: 3` whatever the filter says, and a `min` alone
# `maxRecord: Infinity` (with `{_id: {$gt: 1}}` as the filter, `min {_id:
# 2}` gives `minRecord: 2`). This server bounds the scan by the filter and
# ignores both.


@pytest.mark.backlog("RM-818")
def test_min_and_max_bound_the_clustered_scan(rust_mongo: Any) -> None:
    db = _collection(rust_mongo)
    cursor = db.c.find({"_id": {"$gt": 0}}).hint([("_id", 1)]).min([("_id", 2)]).max([("_id", 4)])
    plan = cursor.explain()["queryPlanner"]["winningPlan"]
    assert (plan["stage"], plan["minRecord"], plan["maxRecord"]) == ("CLUSTERED_IXSCAN", 2, 4)


# The plan cache of a clustered collection (mongod 8.2.11, 2026-11-26): a
# `find` that mongod tries two plans for -- `{_id: {$gt: 1}, x: 1}` beside
# `x_1`, which wins -- leaves an entry in `$planCacheStats` that is active
# after the second run. A read of a clustered collection goes through no
# trial here (`Storage::can_stream` is false for it), so it caches nothing.


@pytest.mark.backlog("RM-819")
def test_a_contested_read_of_a_clustered_collection_is_cached(rust_mongo: Any) -> None:
    db = _collection(rust_mongo, range(1, 11))
    db.c.create_index("x")
    for _ in range(3):
        list(db.c.find({"_id": {"$gt": 1}, "x": 3}))
    entries = list(db.c.aggregate([{"$planCacheStats": {}}]))
    assert [(e["createdFromQuery"]["query"], e["isActive"]) for e in entries] == [
        ({"_id": {"$gt": 1}, "x": 3}, True)
    ]


# A `$group` the SBE runs over an `_id` range (mongod 8.2.11, 2026-11-26)
# is `GROUP <- CLUSTERED_IXSCAN` (minRecord 1, maxRecord Infinity, the
# filter empty) over a `scan` of the slot plan with `minRecordIdSlot` and
# `maxRecordIdSlot`, reading 4 of the 5 documents for `$gt: 1`; this server
# runs it as the `find`'s plan under a `$cursor` stage, `$group` apart.


@pytest.mark.backlog("RM-820")
def test_a_group_over_an_id_range_scans_the_clustered_range(rust_mongo: Any) -> None:
    db = _collection(rust_mongo)
    explained = db.command(
        {
            "explain": {
                "aggregate": "c",
                "pipeline": [
                    {"$match": {"_id": {"$gt": 1}}},
                    {"$group": {"_id": None, "n": {"$sum": 1}}},
                ],
                "cursor": {},
            },
            "verbosity": "executionStats",
        }
    )
    # The group runs in the SBE: its plan is the explain's own, not a
    # `$cursor` stage's.
    plan = explained.get("queryPlanner", {}).get("winningPlan", {})
    scan = plan.get("queryPlan", {}).get("inputStage", {})
    assert (scan.get("stage"), scan.get("minRecord"), scan.get("maxRecord")) == (
        "CLUSTERED_IXSCAN",
        1,
        float("inf"),
    )
    assert explained["executionStats"]["totalDocsExamined"] == 4


# A hint names a clustered collection's key by the name it was created with
# (mongod 8.2.11, 2026-11-26: `clusteredIndex: {name: "ci"}` is hinted `"ci"`
# or `{_id: 1}`, and `"_id_"` is refused, BadValue "hint provided does not
# correspond to an existing index"). `explain` agrees since RM-739; a `find`
# resolves it in the store, which knows the `_id` index as `_id_` alone.


@pytest.mark.backlog("RM-821")
def test_a_clustered_key_is_hinted_by_its_own_name(rust_mongo: Any) -> None:
    db = rust_mongo[f"cl_{uuid.uuid4().hex[:12]}"]
    db.create_collection("c", clusteredIndex={**_SPEC, "name": "ci"})
    db.c.insert_many([{"_id": i} for i in (3, 1, 2)])
    try:
        found: Any = [d["_id"] for d in db.c.find().hint("ci")]
    except Exception as err:  # noqa: BLE001 -- the refusal is the answer
        found = f"refused: {err}"
    assert found == [1, 2, 3]
    with pytest.raises(Exception, match="hint provided does not correspond"):
        list(db.c.find().hint("_id_"))
