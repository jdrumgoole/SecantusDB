"""Divergences found while putting RETURN_KEY at the top of a sorted plan
(RM-470, 2026-11-12), each measured against mongod 8.2.11."""

from __future__ import annotations

import uuid
from typing import Any

import pytest


def _answer(client: Any, command: dict) -> tuple[list, list]:
    """The winning plan's stages, top down, with each SORT's `type`, and
    the documents the command returns."""
    db = client["rm_found_470_" + uuid.uuid4().hex[:8]]
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(12)])
    db.c.create_index([("y", 1), ("x", 1)])
    try:
        plan = db.command("explain", command, verbosity="queryPlanner")["queryPlanner"]
        node, stages = plan["winningPlan"], []
        while isinstance(node, dict):
            stages.append((node["stage"], node.get("type")))
            node = node.get("inputStage") or (node.get("inputStages") or [None])[0]
        return stages, db.command(command)["cursor"]["firstBatch"]
    finally:
        client.drop_database(db.name)


@pytest.mark.backlog_fixed("RM-470")
def test_return_key_keeps_the_residual_filter(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 filters `z` in a FETCH below the sort of a `returnKey`
    read over `{y: 1, x: 1}` and returns the six keys whose documents have
    `z: 1`. This server put RETURN_KEY straight over the IXSCAN, with no
    FETCH and so no filter, and returned all twelve keys."""
    command = {
        "find": "c",
        "filter": {"y": {"$gte": 0}, "z": 1},
        "sort": {"z": 1},
        "returnKey": True,
    }
    expected = [{"y": y, "x": x} for y, x in ((0, 3), (0, 9), (1, 1), (1, 7), (2, 5), (2, 11))]
    answers = {
        server: _answer(client, command)[1]
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == expected
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-566")
def test_a_sort_under_return_key_is_a_default_sort(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SORT under a RETURN_KEY is `type: "default"` (it
    keeps each member's index key), and its top-two of
    `find({y: {$gte: 0}}).sort({z: 1}).limit(2)` returns `x` 6 before 0,
    which tie on `z`. This server's SORT is `simple`, and it returns 0
    before 6."""
    command = {
        "find": "c",
        "filter": {"y": {"$gte": 0}},
        "sort": {"z": 1},
        "limit": 2,
        "returnKey": True,
    }
    answers = {
        server: _answer(client, command)
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == (
        [("RETURN_KEY", None), ("SORT", "default"), ("FETCH", None), ("IXSCAN", None)],
        [{"y": 0, "x": 6}, {"y": 0, "x": 0}],
    )
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-567")
def test_return_key_of_an_id_point_lookup_is_idhack(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers `find({_id: 3}).returnKey()` by IDHACK
    (`RETURN_KEY <- IDHACK`); this server plans `RETURN_KEY <- IXSCAN`."""
    command = {"find": "c", "filter": {"_id": 3}, "returnKey": True}
    answers = {
        server: _answer(client, command)
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == ([("RETURN_KEY", None), ("IDHACK", None)], [{"_id": 3}])
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-568")
def test_return_key_keeps_a_sort_key_meta_field(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps a projection's `{$meta: "sortKey"}` fields under
    `returnKey` (`extractSortKeyMetaFieldsFromProjection`): over a COLLSCAN
    each result is `{k: [0]}`. This server drops the field and returns the
    sort's own field, `{z: 0}`."""
    command = {
        "find": "c",
        "filter": {"z": 0},
        "sort": {"z": 1},
        "projection": {"k": {"$meta": "sortKey"}},
        "returnKey": True,
    }
    answers = {
        server: _answer(client, command)[1]
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == [{"k": [0]}] * 6
    assert answers["rust"] == answers["mongod"]
