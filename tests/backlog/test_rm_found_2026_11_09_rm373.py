"""Divergences found while placing a read's projection where mongod's plan
puts it (RM-373, 2026-11-09), each measured against mongod 8.2.11."""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.backlog.opdebug import logged


def _seed(db: Any) -> None:
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(12)])
    db.c.create_index([("y", 1), ("x", 1)])


def _plan(client: Any, command: dict, verbosity: str = "queryPlanner") -> dict:
    db = client["rm_found_373_" + uuid.uuid4().hex[:8]]
    _seed(db)
    try:
        return db.command("explain", command, verbosity=verbosity)
    finally:
        client.drop_database(db.name)


def _chain(plan: Any, *keys: str) -> list:
    out = []
    while isinstance(plan, dict):
        out.append(tuple(plan.get(k) for k in keys) if keys else plan.get("stage"))
        plan = plan.get("inputStage") or (plan.get("inputStages") or [None])[0]
    return out


def _sort_stage(stats: Any) -> dict:
    while isinstance(stats, dict):
        if stats.get("stage") == "SORT":
            return stats
        stats = stats.get("inputStage")
    raise AssertionError("no SORT stage")


@pytest.mark.backlog_fixed("RM-470")
def test_return_key_is_the_top_of_a_sorted_plan(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 puts RETURN_KEY above a blocking sort: `RETURN_KEY <-
    SORT <- COLLSCAN` for a read no index serves (its projection dropped),
    `RETURN_KEY <- SORT <- FETCH <- IXSCAN` for one whose index does not
    hold the sort. This server ignored `returnKey` over a COLLSCAN
    (`PROJECTION_SIMPLE <- SORT <- COLLSCAN`) and put RETURN_KEY below the
    sort, with no FETCH (`SORT <- RETURN_KEY <- IXSCAN`)."""
    commands = {
        "collscan": {
            "find": "c",
            "filter": {"z": 0},
            "projection": {"z": 1},
            "sort": {"z": 1},
            "returnKey": True,
        },
        "ixscan": {"find": "c", "filter": {"y": {"$gte": 0}}, "sort": {"z": 1}, "returnKey": True},
    }
    answers = {
        server: {
            n: _chain(_plan(client, c)["queryPlanner"]["winningPlan"]) for n, c in commands.items()
        }
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == {
        "collscan": ["RETURN_KEY", "SORT", "COLLSCAN"],
        "ixscan": ["RETURN_KEY", "SORT", "FETCH", "IXSCAN"],
    }
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-471")
def test_an_or_of_scans_answers_a_covered_projection(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers `{_id: 0, x: 1}` over an `$or` of two scans of
    `{y: 1, x: 1}` from their keys: `SUBPLAN <- PROJECTION_DEFAULT <- OR <-
    IXSCAN`, no FETCH. This server fetched each document
    (`PROJECTION_SIMPLE <- FETCH <- OR`) until 2026-11-12."""
    command = {
        "find": "c",
        "filter": {"$or": [{"y": 0}, {"y": 1, "x": {"$gt": 3}}]},
        "projection": {"_id": 0, "x": 1},
    }
    answers = {
        server: _chain(_plan(client, command)["queryPlanner"]["winningPlan"])
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == ["SUBPLAN", "PROJECTION_DEFAULT", "OR", "IXSCAN"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-472")
def test_a_sort_beside_show_record_id_is_a_default_sort(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SORT under a `showRecordId` read's projection is
    `type: "default"` -- it keeps each document's RecordId -- where the same
    read without `showRecordId` sorts `simple`. This server's was `simple`
    both ways."""
    command = {
        "find": "c",
        "filter": {"z": 0},
        "projection": {"z": 1},
        "sort": {"z": 1},
        "showRecordId": True,
    }
    answers = {
        server: _chain(_plan(client, command)["queryPlanner"]["winningPlan"], "stage", "type")
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == [
        ("PROJECTION_DEFAULT", None),
        ("SORT", "default"),
        ("COLLSCAN", None),
    ]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-472")
def test_a_sort_beside_a_metadata_projection_is_a_default_sort(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's SORT is `simple` only when the read depends on no
    metadata but `sortKey` (`canUseSimpleSort`): a projection reading
    `{$meta: "recordId"}`, `"indexKey"` or `"randVal"` sorts `default`, over
    a COLLSCAN and over a FETCH, and one reading only `"sortKey"` sorts
    `simple`. This server's was `simple` for all of them."""
    metas = {"recordId": {"z": 0}, "indexKey": {"y": 1}, "randVal": {"z": 0}, "sortKey": {"z": 0}}
    answers = {
        server: {
            meta: [
                stage
                for stage in _chain(
                    _plan(
                        client,
                        {
                            "find": "c",
                            "filter": flt,
                            "projection": {"z": 1, "m": {"$meta": meta}},
                            "sort": {"z": 1},
                        },
                    )["queryPlanner"]["winningPlan"],
                    "stage",
                    "type",
                )
                if stage[0] == "SORT"
            ]
            for meta, flt in metas.items()
        }
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == {
        "recordId": [("SORT", "default")],
        "indexKey": [("SORT", "default")],
        "randVal": [("SORT", "default")],
        "sortKey": [("SORT", "simple")],
    }
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-473")
def test_a_sort_over_a_covered_projection_counts_what_it_sorted(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's `SORT <- PROJECTION_COVERED <- IXSCAN` for
    `find({y: {$gte: 0}}, {_id: 0, x: 1}).sort({x: 1})` (`type:
    "default"`) counts 1584 bytes sorted for its twelve members, in
    `explain` and in a profile entry's `execStats`: each an OWNED_OBJ
    working-set member, its sort key and its document's approximate size.
    This server counted 780 in both, the members of their index keys, as
    its sort of keys below a FETCH counts them."""

    def op(d: Any) -> Any:
        return list(d.c.find({"y": {"$gte": 0}}, {"_id": 0, "x": 1}).sort("x", 1))

    command = {
        "find": "c",
        "filter": {"y": {"$gte": 0}},
        "projection": {"_id": 0, "x": 1},
        "sort": {"x": 1},
    }

    def sizes(client: Any, is_mongod: bool) -> list:
        explained = _plan(client, command, "executionStats")["executionStats"]["executionStages"]
        entries = logged(client, is_mongod, _seed, op)[1]
        return [_sort_stage(explained).get("totalDataSizeSorted")] + [
            _sort_stage(e["execStats"]).get("totalDataSizeSorted")
            for e in entries
            if "execStats" in e
        ]

    answers = {"mongod": sizes(mongod, True), "rust": sizes(rust_mongo, False)}
    assert answers["mongod"] == [1584, 1584]
    assert answers["rust"] == answers["mongod"]
