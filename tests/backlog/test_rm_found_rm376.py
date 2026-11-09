"""Writes planned as mongod plans them (RM-376), and a divergence found
while working it (2026-11-09), each measured against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.opdebug import logged, seed


def _default_fr(d: Any) -> None:
    d.create_collection("c", collation={"locale": "fr"})
    d.c.insert_many([{"_id": n, "y": n} for n in range(4)])


def _plans(client: Any, is_mongod: bool, ops: dict) -> dict:
    out = {}
    for name, (setup, op) in ops.items():
        entries = logged(client, is_mongod, setup, op)[1]
        out[name] = [
            (e.get("planSummary"), e.get("keysExamined"), e.get("docsExamined")) for e in entries
        ]
    return out


@pytest.mark.backlog_fixed("RM-376")
def test_a_write_takes_the_id_fast_path_only_under_the_id_indexs_collation(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 writes by `_id` without its planner only when the
    statement's collation is the `_id` index's (the collection's default).
    Under another, a number's bounds read the index and a string's do not,
    an upsert, a delete and a findAndModify included. This server took
    the express path under any collation, and gave the `_id` index of a
    collection with a default collation none of its own."""
    fr = {"locale": "fr"}
    simple = {"locale": "simple"}
    ops = {
        "string": (seed, lambda d: d.c.update_one({"_id": "a"}, {"$set": {"w": 1}}, collation=fr)),
        "upsert": (
            seed,
            lambda d: d.c.update_one({"_id": 3}, {"$set": {"w": 1}}, collation=fr, upsert=True),
        ),
        "simple": (
            seed,
            lambda d: d.c.update_one({"_id": 3}, {"$set": {"w": 1}}, collation=simple),
        ),
        "delete": (seed, lambda d: d.c.delete_one({"_id": 3}, collation=fr)),
        "findAndModify": (
            seed,
            lambda d: d.c.find_one_and_update({"_id": 3}, {"$set": {"w": 1}}, collation=fr),
        ),
        "default": (_default_fr, lambda d: d.c.update_one({"_id": 3}, {"$set": {"w": 1}})),
        "default-simple": (
            _default_fr,
            lambda d: d.c.update_one({"_id": 3}, {"$set": {"w": 1}}, collation=simple),
        ),
    }
    express = ("EXPRESS_IXSCAN { _id: 1 },EXPRESS_UPDATE", 1, 1)
    ixscan = ("IXSCAN { _id: 1 }", 1, 1)
    answers = {"mongod": _plans(mongod, True, ops), "rust": _plans(rust_mongo, False, ops)}
    assert answers["mongod"] == {
        "string": [("COLLSCAN", 0, 12)],
        "upsert": [ixscan],
        "simple": [express],
        "delete": [ixscan],
        "findAndModify": [ixscan],
        "default": [express],
        "default-simple": [ixscan],
    }
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-376")
def test_an_update_reads_under_its_sort(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reads an update with a `sort` (8.0) in the sort's
    order: an index that gives it (`IXSCAN { x: 1 }`, one key and one
    document), a SORT over every match when none does, nothing matched and
    the upsert after it included; an `_id` point query still takes the
    express path. This server pinned the update to the document that sorts
    first and reported an `_id` express write for every one."""

    def update(q: dict, sort: dict, upsert: bool = False) -> Any:
        return lambda d: d.command(
            "update",
            "c",
            updates=[{"q": q, "u": {"$set": {"w": 1}}, "sort": sort, "upsert": upsert}],
        )

    ops = {
        "index": (seed, update({"x": {"$gte": 2}}, {"x": 1})),
        "id": (seed, update({"_id": 3}, {"y": 1})),
        "upsert": (seed, update({"z": 7}, {"y": 1}, upsert=True)),
    }
    answers = {"mongod": _plans(mongod, True, ops), "rust": _plans(rust_mongo, False, ops)}
    assert answers["mongod"] == {
        "index": [("IXSCAN { x: 1 }", 1, 1)],
        "id": [("EXPRESS_IXSCAN { _id: 1 },EXPRESS_UPDATE", 1, 1)],
        "upsert": [("COLLSCAN", 0, 12)],
    }
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-474")
def test_a_writes_limit_of_one_is_done_after_one_work(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's LIMIT of one, over an index read in a write's sort
    order, is EOF after the one work that returned its document: the
    profile entry's UPDATE and LIMIT each report `works: 1`, for an update
    with a `sort` and a findAndModify alike. This server's report two, and
    the UPDATE a `needTime` of 1."""
    ops = {
        "update": (
            seed,
            lambda d: d.command(
                "update",
                "c",
                updates=[{"q": {"x": {"$gte": 2}}, "u": {"$set": {"w": 1}}, "sort": {"x": 1}}],
            ),
        ),
        "findAndModify": (
            seed,
            lambda d: d.c.find_one_and_update(
                {"x": {"$gte": 2}}, {"$set": {"w": 1}}, sort=[("x", 1)]
            ),
        ),
    }

    def works(client: Any, is_mongod: bool) -> dict:
        out = {}
        for name, (setup, op) in ops.items():
            stats = [e["execStats"] for e in logged(client, is_mongod, setup, op)[1]]
            out[name] = [
                (s["works"], s["needTime"], s["inputStage"]["stage"], s["inputStage"]["works"])
                for s in stats
            ]
        return out

    answers = {"mongod": works(mongod, True), "rust": works(rust_mongo, False)}
    assert answers["mongod"] == {
        "update": [(1, 0, "LIMIT", 1)],
        "findAndModify": [(1, 0, "LIMIT", 1)],
    }
    assert answers["rust"] == answers["mongod"]
