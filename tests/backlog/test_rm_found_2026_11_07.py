"""Divergences found while giving profile entries their `execStats`
(RM-359, 2026-11-07), each measured against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.opdebug import logged, seed


def _entries(client: Any, is_mongod: bool, op: Any, setup: Any = seed) -> list[dict]:
    return logged(client, is_mongod, setup, op)[1]


def _sort_stage(stats: Any) -> dict:
    while isinstance(stats, dict):
        if stats.get("stage") == "SORT":
            return stats
        stats = stats.get("inputStage")
    raise AssertionError("no SORT stage")


@pytest.mark.backlog("RM-377")
def test_a_distinct_scan_examines_the_key_past_its_bounds(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's DISTINCT_SCAN for `distinct("x", {x: {$lt: 4}})` over
    keys 0 to 11 examines five keys: the four it returns and the 4 it seeks
    to and finds past its bounds (the entry's `keysExamined`, and its
    `execStats`'). This server's examines four."""

    def op(d: Any) -> Any:
        return d.c.distinct("x", {"x": {"$lt": 4}})

    answers = {
        "mongod": [e.get("keysExamined") for e in _entries(mongod, True, op)],
        "rust": [e.get("keysExamined") for e in _entries(rust_mongo, False, op)],
    }
    assert answers["mongod"] == [5]
    assert answers["rust"] == answers["mongod"]


def _two_indexes(d: Any) -> None:
    d.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(12)])
    d.c.create_index([("y", 1), ("x", 1)])


def _chain(client: Any, command: dict) -> list:
    db = client["rm_found_projection"]
    db.drop_collection("c")
    _two_indexes(db)
    plan = db.command("explain", command, verbosity="queryPlanner")["queryPlanner"]["winningPlan"]
    client.drop_database("rm_found_projection")
    out = []
    while isinstance(plan, dict):
        out.append(plan.get("stage"))
        plan = plan.get("inputStage") or (plan.get("inputStages") or [None])[0]
    return out


@pytest.mark.backlog("RM-373")
def test_a_projection_is_where_mongods_plan_puts_it(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 projects BELOW a blocking SORT (`SORT` over
    `PROJECTION_SIMPLE` / `PROJECTION_COVERED`), and the projection of a
    SORT_MERGE whose keys hold the projected fields is `PROJECTION_DEFAULT`.
    This server projects above the SORT, and calls the merge's projection
    covered."""
    commands = {
        "simple": {"find": "c", "filter": {"z": 0}, "projection": {"y": 1}, "sort": {"y": 1}},
        "covered": {
            "find": "c",
            "filter": {"y": {"$gte": 0}},
            "projection": {"_id": 0, "x": 1},
            "sort": {"x": 1},
        },
        "merge": {
            "find": "c",
            "filter": {"y": {"$in": [0, 1]}},
            "projection": {"_id": 0, "x": 1, "y": 1},
            "sort": {"x": 1},
        },
    }
    answers = {
        server: {name: _chain(client, c) for name, c in commands.items()}
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == {
        "simple": ["SORT", "PROJECTION_SIMPLE", "COLLSCAN"],
        "covered": ["SORT", "PROJECTION_COVERED", "IXSCAN"],
        "merge": ["PROJECTION_DEFAULT", "SORT_MERGE", "IXSCAN"],
    }
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-374")
def test_a_sort_of_one_document_counts_no_data_sorted(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SORT with a limit of 1 keeps the one best document
    and counts no data sorted (`totalDataSizeSorted: 0`, a `find`'s and a
    `findAndModify`'s alike; 201 for a limit of 3). This server's sorter
    counts the documents it kept (67)."""

    def op(d: Any) -> Any:
        return list(d.c.find({"z": 0}).sort("y", 1).limit(1))

    answers = {
        "mongod": [
            _sort_stage(e["execStats"])["totalDataSizeSorted"] for e in _entries(mongod, True, op)
        ],
        "rust": [
            _sort_stage(e.get("execStats"))["totalDataSizeSorted"]
            for e in _entries(rust_mongo, False, op)
        ],
    }
    assert answers["mongod"] == [0]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-375")
def test_a_sort_of_index_keys_counts_the_data_it_sorted(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SORT of index keys below a FETCH (`type:
    "default"`) counts the data it sorted: 780 for the twelve `{y, x}`
    keys of `find({y: {$gte: 0}}).sort({x: 1})`. This server sorts the keys
    without its sorter and reports no `totalDataSizeSorted`."""

    def op(d: Any) -> Any:
        return list(d.c.find({"y": {"$gte": 0}}).sort("x", 1))

    answers = {
        "mongod": [
            _sort_stage(e["execStats"]).get("totalDataSizeSorted")
            for e in _entries(mongod, True, op, _two_indexes)
        ],
        "rust": [
            _sort_stage(e.get("execStats")).get("totalDataSizeSorted")
            for e in _entries(rust_mongo, False, op, _two_indexes)
        ],
    }
    assert answers["mongod"] == [780]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-376")
def test_an_update_is_planned_as_mongod_plans_it(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 plans an update with a `sort` over its read (a
    COLLSCAN here, 0 keys and 12 documents examined) and an `_id` update
    under a non-simple collation through the `_id` index (`IXSCAN { _id: 1
    }`, one key and one document). This server's statements report an
    `_id` express write for both."""
    ops = {
        "sort": lambda d: d.command(
            "update", "c", updates=[{"q": {"z": 0}, "u": {"$set": {"w": 1}}, "sort": {"y": -1}}]
        ),
        "collation": lambda d: d.c.update_one(
            {"_id": 3}, {"$set": {"w": 1}}, collation={"locale": "fr"}
        ),
    }

    def plans(client: Any, is_mongod: bool) -> dict:
        return {
            name: [
                (e.get("planSummary"), e.get("keysExamined"), e.get("docsExamined"))
                for e in _entries(client, is_mongod, op)
            ]
            for name, op in ops.items()
        }

    answers = {"mongod": plans(mongod, True), "rust": plans(rust_mongo, False)}
    assert answers["mongod"] == {
        "sort": [("COLLSCAN", 0, 12)],
        "collation": [("IXSCAN { _id: 1 }", 1, 1)],
    }
    assert answers["rust"] == answers["mongod"]
