"""Where a read's projection runs relative to its blocking SORT, and what
the SORT then counts (RM-373).

mongod 8.2 runs a projection below a blocking sort when the sort has no
limit, the projection computes nothing (every leaf an include or exclude
flag; a dotted or positional path still counts, a literal, an expression,
`$slice`, `$elemMatch`, `$meta` or `showRecordId` does not), and it hands
on whole every path the sort orders by. The sort then orders, and counts,
the projected documents. A projection with a literal is
`PROJECTION_DEFAULT`, and so is a covered one over a SORT_MERGE. Compared
are the winning plan's stages (with `transformBy`, the sort's pattern and
type) and the SORT's `totalDataSizeSorted`, in `explain` and in a profile
entry's `execStats`. Measured on 8.2.11, 2026-11-09.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.backlog.opdebug import logged
from tests.mongod_live.conftest import same

_COVERING = {"y": {"$gte": 0}}
_MERGING = {"y": {"$in": [0, 1]}}

FINDS: dict[str, dict] = {
    "inclusion": {"filter": {"z": 0}, "projection": {"y": 1, "z": 1}, "sort": {"z": 1}},
    "inclusion_drops_sort": {"filter": {"z": 0}, "projection": {"y": 1}, "sort": {"z": 1}},
    "id_only": {"filter": {"z": 0}, "projection": {"_id": 1}, "sort": {"z": 1}},
    "no_id": {"filter": {"z": 0}, "projection": {"_id": 0, "z": 1}, "sort": {"z": 1}},
    "no_id_sort_id": {"filter": {"z": 0}, "projection": {"_id": 0, "z": 1}, "sort": {"_id": -1}},
    "bool_and_double": {"filter": {"z": 0}, "projection": {"z": True, "y": 1.0}, "sort": {"z": 1}},
    "limit": {"filter": {"z": 0}, "projection": {"z": 1}, "sort": {"z": 1}, "limit": 2},
    "skip": {"filter": {"z": 0}, "projection": {"z": 1}, "sort": {"z": 1}, "skip": 2},
    "skip_limit": {
        "filter": {"z": 0},
        "projection": {"z": 1},
        "sort": {"z": 1},
        "skip": 1,
        "limit": 2,
    },
    "exclusion": {"filter": {"z": 0}, "projection": {"x": 0}, "sort": {"z": 1}},
    "exclusion_drops_sort": {"filter": {"z": 0}, "projection": {"z": 0}, "sort": {"z": 1}},
    "exclusion_of_id": {"filter": {"z": 0}, "projection": {"_id": 0}, "sort": {"_id": 1}},
    "computed": {"filter": {"z": 0}, "projection": {"z": 1, "w": "$x"}, "sort": {"z": 1}},
    "literal": {"filter": {"z": 0}, "projection": {"z": 1, "q": None}, "sort": {"z": 1}},
    "literal_id": {"filter": {"z": 0}, "projection": {"z": 1, "_id": "x"}, "sort": {"z": 1}},
    "slice": {"filter": {"z": 0}, "projection": {"z": 1, "t": {"$slice": 1}}, "sort": {"z": 1}},
    "elem_match": {
        "filter": {"z": 0},
        "projection": {"z": 1, "t": {"$elemMatch": {"$gt": 1}}},
        "sort": {"z": 1},
    },
    "meta": {
        "filter": {"z": 0},
        "projection": {"z": 1, "k": {"$meta": "sortKey"}},
        "sort": {"z": 1},
    },
    "positional": {"filter": {"z": 0, "t": 2}, "projection": {"z": 1, "t.$": 1}, "sort": {"z": 1}},
    "dotted": {"filter": {"z": 0}, "projection": {"z": 1, "a.b": 1}, "sort": {"z": 1}},
    "sub_document": {"filter": {"z": 0}, "projection": {"a": {"b": 1}, "z": 1}, "sort": {"z": 1}},
    "dotted_sort": {"filter": {"z": 0}, "projection": {"a.b": 1}, "sort": {"a.b": 1}},
    "parent_of_sort": {"filter": {"z": 0}, "projection": {"a": 1}, "sort": {"a.b": 1}},
    "child_of_sort": {"filter": {"z": 0}, "projection": {"a.b": 1}, "sort": {"a": 1}},
    "exclusion_beside_sort": {"filter": {"z": 0}, "projection": {"a.c": 0}, "sort": {"a.b": 1}},
    "exclusion_of_sort_child": {"filter": {"z": 0}, "projection": {"a.b": 0}, "sort": {"a": 1}},
    "covered": {"filter": _COVERING, "projection": {"_id": 0, "x": 1}, "sort": {"x": 1}},
    "covered_drops_sort": {"filter": _COVERING, "projection": {"_id": 0, "y": 1}, "sort": {"x": 1}},
    "covered_limit": {
        "filter": _COVERING,
        "projection": {"_id": 0, "x": 1},
        "sort": {"x": 1},
        "limit": 3,
    },
    "covered_skip": {
        "filter": _COVERING,
        "projection": {"_id": 0, "x": 1},
        "sort": {"x": 1},
        "skip": 3,
    },
    "keys_sort_below_fetch": {"filter": _COVERING, "projection": {"x": 1}, "sort": {"x": 1}},
    "fetched": {"filter": _COVERING, "projection": {"z": 1}, "sort": {"z": 1}},
    "merge_covered": {
        "filter": _MERGING,
        "projection": {"_id": 0, "x": 1, "y": 1},
        "sort": {"x": 1},
    },
    "merge_fetched": {"filter": _MERGING, "projection": {"x": 1}, "sort": {"x": 1}},
}

PIPELINES: dict[str, list[dict]] = {
    "sort_project": [{"$match": {"z": 0}}, {"$sort": {"z": 1}}, {"$project": {"z": 1}}],
    "sort_limit_project": [
        {"$match": {"z": 0}},
        {"$sort": {"z": 1}},
        {"$limit": 2},
        {"$project": {"z": 1}},
    ],
    "sort_skip_project": [
        {"$match": {"z": 0}},
        {"$sort": {"z": 1}},
        {"$skip": 1},
        {"$project": {"z": 1}},
    ],
    "covered": [{"$match": _COVERING}, {"$sort": {"x": 1}}, {"$project": {"_id": 0, "x": 1}}],
    "merge": [{"$match": _MERGING}, {"$sort": {"x": 1}}, {"$project": {"_id": 0, "x": 1}}],
}

#: The reads whose SORT sorts documents (`type: "simple"`): what it counts
#: is the projected documents' size.
SIZED = ["inclusion", "no_id", "skip", "exclusion", "positional", "dotted", "fetched"]


def _seed(db: Any) -> None:
    db.c.insert_many(
        [
            {"_id": n, "x": n, "y": n % 3, "z": n % 2, "a": {"b": n, "c": n}, "t": [1, 2, 3]}
            for n in range(12)
        ]
    )
    db.c.create_index([("y", 1), ("x", 1)])


def _strip(plan: Any) -> Any:
    out = []
    while isinstance(plan, dict):
        out.append(
            {k: plan[k] for k in ("stage", "transformBy", "sortPattern", "type") if k in plan}
        )
        plan = plan.get("inputStage") or (plan.get("inputStages") or [None])[0]
    return out


def _explained(client: Any, command: dict, verbosity: str = "queryPlanner") -> dict:
    db = client["projection_below_sort_" + uuid.uuid4().hex[:8]]
    _seed(db)
    try:
        e = db.command("explain", {**command}, verbosity=verbosity)
    finally:
        client.drop_database(db.name)
    return e if "queryPlanner" in e else e["stages"][0]["$cursor"]


def _sort_stage(stats: Any) -> dict:
    while isinstance(stats, dict):
        if stats.get("stage") == "SORT":
            return stats
        stats = stats.get("inputStage")
    raise AssertionError("no SORT stage")


@pytest.mark.parametrize("name", list(FINDS))
def test_a_finds_projection_is_where_mongods_plan_puts_it(
    rust_mongo: Any, mongod: Any, name: str
) -> None:
    command = {"find": "c", **FINDS[name]}
    answers = [
        _strip(_explained(client, command)["queryPlanner"]["winningPlan"])
        for client in (rust_mongo, mongod)
    ]
    same(*answers)


@pytest.mark.parametrize("sort", [{"z": 1}, None])
def test_a_projection_beside_show_record_id_names_the_record_id(
    rust_mongo: Any, mongod: Any, sort: dict | None
) -> None:
    """`showRecordId` adds `$recordId` to the projection, which makes it
    `PROJECTION_DEFAULT` and keeps it above a sort. (The SORT's own type
    beside it is RM-472's.)"""
    command = {"find": "c", "filter": {"z": 0}, "projection": {"z": 1}, "showRecordId": True}
    if sort:
        command["sort"] = sort
    answers = [
        [
            {k: v for k, v in stage.items() if k != "type"}
            for stage in _strip(_explained(client, command)["queryPlanner"]["winningPlan"])
        ]
        for client in (rust_mongo, mongod)
    ]
    same(*answers)


@pytest.mark.parametrize("name", list(PIPELINES))
def test_an_aggregates_projection_is_where_mongods_plan_puts_it(
    rust_mongo: Any, mongod: Any, name: str
) -> None:
    command = {"aggregate": "c", "pipeline": PIPELINES[name], "cursor": {}}
    answers = [
        _strip(_explained(client, command)["queryPlanner"]["winningPlan"])
        for client in (rust_mongo, mongod)
    ]
    same(*answers)


@pytest.mark.parametrize("name", SIZED)
def test_a_sort_over_a_projection_counts_the_projected_documents(
    rust_mongo: Any, mongod: Any, name: str
) -> None:
    command = {"find": "c", **FINDS[name]}

    def explained(client: Any) -> Any:
        stats = _explained(client, command, "executionStats")["executionStats"]
        return _sort_stage(stats["executionStages"])["totalDataSizeSorted"]

    def profiled(client: Any, is_mongod: bool) -> list:
        spec = FINDS[name]

        def op(d: Any) -> Any:
            cursor = d.c.find(spec["filter"], spec["projection"]).sort(list(spec["sort"].items()))
            return list(cursor.skip(spec.get("skip", 0)))

        entries = logged(client, is_mongod, _seed, op)[1]
        return [
            _sort_stage(e["execStats"])["totalDataSizeSorted"] for e in entries if "execStats" in e
        ]

    same(
        [explained(rust_mongo), profiled(rust_mongo, False)],
        [explained(mongod), profiled(mongod, True)],
    )


#: Covered reads whose SORT sorts what `PROJECTION_COVERED` built of the
#: keys (`SORT <- PROJECTION_COVERED <- IXSCAN`, `type: "default"`): each
#: an OWNED_OBJ working-set member, counted as its sort key and its
#: document's `getApproximateSize` (RM-473).
COVERED: dict[str, dict] = {
    "x": {"projection": {"_id": 0, "x": 1}, "sort": {"x": 1}},
    "x_descending": {"projection": {"_id": 0, "x": 1}, "sort": {"x": -1}},
    "x_and_y": {"projection": {"_id": 0, "x": 1, "y": 1}, "sort": {"x": 1}},
    "compound_sort": {"projection": {"_id": 0, "x": True, "y": 1.0}, "sort": {"x": 1, "y": -1}},
    "skip": {"projection": {"_id": 0, "x": 1}, "sort": {"x": 1}, "skip": 3},
}


@pytest.mark.parametrize("name", list(COVERED))
def test_a_sort_over_a_covered_projection_counts_its_documents(
    rust_mongo: Any, mongod: Any, name: str
) -> None:
    spec = COVERED[name]
    command = {"find": "c", "filter": _COVERING, **spec}

    def explained(client: Any) -> Any:
        stats = _explained(client, command, "executionStats")["executionStats"]
        stage = _sort_stage(stats["executionStages"])
        return [stage["type"], stage["totalDataSizeSorted"]]

    def profiled(client: Any, is_mongod: bool) -> list:
        def op(d: Any) -> Any:
            cursor = d.c.find(_COVERING, spec["projection"]).sort(list(spec["sort"].items()))
            return list(cursor.skip(spec.get("skip", 0)))

        entries = logged(client, is_mongod, _seed, op)[1]
        return [
            _sort_stage(e["execStats"])["totalDataSizeSorted"] for e in entries if "execStats" in e
        ]

    same(
        [explained(rust_mongo), profiled(rust_mongo, False)],
        [explained(mongod), profiled(mongod, True)],
    )


@pytest.mark.parametrize("projection", [{"_id": 0, "x": 1}, {"_id": 0, "x": 1, "y": 1}])
def test_a_sort_over_a_covered_projection_spills_its_documents(
    rust_mongo: Any, mongod: Any, projection: dict
) -> None:
    """2000 documents under a 100000-byte budget with `allowDiskUse`: the
    members spill as OWNED_OBJ working-set members (mongod 8.2.11: 108000
    bytes for `{x}`, in 5 spills of 4000 records). What the runs compress
    to (`spilledDataStorageSize`) is left out: mongod's members carry
    snapshot ids from a process-wide counter (RM-556)."""

    def spilled(client: Any, is_mongod: bool) -> dict:
        client.admin.command("setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=100000)
        try:

            def setup(d: Any) -> None:
                d.c.insert_many([{"_id": i, "x": i, "y": i % 3} for i in range(2000)])
                d.c.create_index([("y", 1), ("x", 1)])

            def op(d: Any) -> None:
                d.command(
                    "find",
                    "c",
                    filter=_COVERING,
                    projection=projection,
                    sort={"x": 1},
                    allowDiskUse=True,
                    batchSize=3000,
                )

            entry = logged(client, is_mongod, setup, op)[1][0]
        finally:
            client.admin.command(
                "setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=104857600
            )
        stage = _sort_stage(entry.get("execStats"))
        keys = ("type", "totalDataSizeSorted", "spills", "spilledRecords", "spilledBytes")
        return {k: stage.get(k) for k in keys}

    answers = {"mongod": spilled(mongod, True), "rust": spilled(rust_mongo, False)}
    assert answers["mongod"]["spills"] > 0, answers
    same(answers["rust"], answers["mongod"])
