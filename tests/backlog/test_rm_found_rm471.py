"""Gap tests for the Rust MongoDB server items found on 2026-11-12 while
working RM-471 (an `$or` of index scans answering a covered projection).

Each test asks mongod 8.2.11 live and asserts the Rust server gives the same
answer.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest


def _db(client: Any, *indexes: Any) -> Any:
    db = client["rm_found_471_" + uuid.uuid4().hex[:8]]
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2, "w": n % 4} for n in range(12)])
    for index in indexes:
        db.c.create_index(index)
    return db


def _chain(plan: Any, *keys: str) -> list:
    out = []
    while isinstance(plan, dict):
        out.append(tuple(plan.get(k) for k in keys) if keys else plan.get("stage"))
        plan = plan.get("inputStage") or (plan.get("inputStages") or [None])[0]
    return out


def _plan(client: Any, command: dict, *indexes: Any) -> dict:
    db = _db(client, *indexes)
    try:
        return db.command("explain", command)["queryPlanner"]["winningPlan"]
    finally:
        client.drop_database(db.name)


def _rows(client: Any, command: dict, *indexes: Any) -> list:
    db = _db(client, *indexes)
    try:
        rows = db.command(command)["cursor"]["firstBatch"]
        return [list(row.items()) for row in rows]
    finally:
        client.drop_database(db.name)


YX = [("y", 1), ("x", 1)]
XY = [("x", 1), ("y", 1)]


@pytest.mark.backlog("RM-569")
def test_covered_fields_come_back_in_the_key_patterns_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 builds a covered row from the index key, so its fields
    come in the key pattern's order: `{_id: 0, x: 1, y: 1}` over `{y: 1,
    x: 1}` returns `{y: .., x: ..}`, from one scan or an `$or` of them.
    This server returns them in the document's order, `{x: .., y: ..}`."""
    commands = {
        "one scan": {"find": "c", "filter": {"y": 0}, "projection": {"_id": 0, "x": 1, "y": 1}},
        "or": {
            "find": "c",
            "filter": {"$or": [{"y": 0}, {"y": 1, "x": {"$gt": 3}}]},
            "projection": {"_id": 0, "x": 1, "y": 1},
        },
    }
    answers = {
        server: {n: _rows(client, c, YX) for n, c in commands.items()}
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"]["one scan"][0] == [("y", 0), ("x", 0)]
    assert answers["mongod"]["or"][0] == [("y", 1), ("x", 4)]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-570")
def test_a_computed_projection_reads_index_keys(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers `{_id: 0, x: 1, k: {$add: ["$x", 1]}}` from the
    keys of `{y: 1, x: 1}`, everything it reads being in the index
    (`PROJECTION_DEFAULT <- IXSCAN`). This server fetches each document
    (`PROJECTION_DEFAULT <- FETCH <- IXSCAN`)."""
    command = {
        "find": "c",
        "filter": {"y": 0},
        "projection": {"_id": 0, "x": 1, "k": {"$add": ["$x", 1]}},
    }
    answers = {
        server: _chain(_plan(client, command, YX))
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == ["PROJECTION_DEFAULT", "IXSCAN"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-571")
def test_return_key_drops_the_projection(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `returnKey` read with a projection is `RETURN_KEY <-
    IXSCAN`, the projection dropped, from one scan or an `$or` of them.
    This server kept a projection over the RETURN_KEY (`PROJECTION_COVERED
    <- RETURN_KEY <- IXSCAN`; over an `$or`, `PROJECTION_DEFAULT`) until
    RM-470's fix dropped a `returnKey` read's projection (2026-11-12)."""
    commands = {
        "one scan": {
            "find": "c",
            "filter": {"y": 0},
            "projection": {"_id": 0, "x": 1},
            "returnKey": True,
        },
        "or": {
            "find": "c",
            "filter": {"$or": [{"y": 0}, {"y": 1, "x": {"$gt": 3}}]},
            "projection": {"_id": 0, "x": 1},
            "returnKey": True,
        },
    }
    answers = {
        server: {n: _chain(_plan(client, c, YX)) for n, c in commands.items()}
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == {
        "one scan": ["RETURN_KEY", "IXSCAN"],
        "or": ["SUBPLAN", "RETURN_KEY", "OR", "IXSCAN"],
    }
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-572")
def test_an_or_of_two_indexes_returns_its_branches_in_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reads `{$or: [{y: 0}, {x: {$gt: 8}}]}` over `{y: 1,
    x: 1}` and `{x: 1}` branch by branch as its plan lists them, `y_1_x_1`
    first: `_id` 0, 3, 6, 9, then 10, 11. This server returns the `x_1`
    branch first (9, 10, 11, 0, 3, 6), the order its own explain does not
    list -- and so, sorted on `y` with a limit, a different two rows."""
    commands = {
        "plain": {"find": "c", "filter": {"$or": [{"y": 0}, {"x": {"$gt": 8}}]}},
        "sorted, limit": {
            "find": "c",
            "filter": {"$or": [{"y": 0}, {"x": {"$gt": 8}}]},
            "projection": {"_id": 0, "x": 1},
            "sort": {"y": 1},
            "limit": 2,
        },
    }
    answers = {
        server: {
            n: [dict(r).get("_id", dict(r).get("x")) for r in _rows(client, c, YX, XY)]
            for n, c in commands.items()
        }
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == {"plain": [0, 3, 6, 9, 10, 11], "sorted, limit": [0, 3]}
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-573")
def test_an_id_projection_is_covered(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers `{_id: 1}` from an index that holds `_id`:
    `PROJECTION_COVERED <- IXSCAN` over `{y: 1, _id: 1}`. This server
    fetches each document (`PROJECTION_SIMPLE <- FETCH <- IXSCAN`)."""
    command = {"find": "c", "filter": {"y": 0}, "projection": {"_id": 1}}
    answers = {
        server: _chain(_plan(client, command, [("y", 1), ("_id", 1)]))
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == ["PROJECTION_COVERED", "IXSCAN"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-574")
def test_a_clause_beside_an_or_is_answered_by_its_bounds(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 pushes `x: {$lt: 10}` beside `{$or: [{y: 0}, {y: 1,
    x: {$gt: 3}}]}` into each branch's bounds over `{y: 1, x: 1}` and
    rechecks nothing, so `{_id: 0, x: 1}` is answered from the keys
    (`PROJECTION_DEFAULT <- OR <- IXSCAN`). This server's bounds are the
    same, but it rechecks the clause in a FETCH (`PROJECTION_SIMPLE <-
    FETCH <- OR`)."""
    command = {
        "find": "c",
        "filter": {"$or": [{"y": 0}, {"y": 1, "x": {"$gt": 3}}], "x": {"$lt": 10}},
        "projection": {"_id": 0, "x": 1},
    }
    answers = {
        server: _chain(_plan(client, command, YX))
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == ["PROJECTION_DEFAULT", "OR", "IXSCAN"]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-575")
def test_a_sort_over_an_or_with_a_fetched_branch_sorts_below_the_fetch(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's OR of `FETCH <- IXSCAN w_1` and `IXSCAN y_1_x_1` is
    not fetched, and every branch carries `x` (a fetched one carries every
    field), so `.sort({x: 1})` sorts the OR's members below a FETCH:
    `SUBPLAN <- FETCH <- SORT (default) <- OR`. This server sorts the
    fetched documents above it: `SUBPLAN <- SORT (simple) <- FETCH <-
    OR`."""
    command = {"find": "c", "filter": {"$or": [{"y": 0}, {"w": 1, "z": 1}]}, "sort": {"x": 1}}
    answers = {
        server: _chain(_plan(client, command, YX, [("w", 1)]), "stage", "type")
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert answers["mongod"] == [
        ("SUBPLAN", None),
        ("FETCH", None),
        ("SORT", "default"),
        ("OR", None),
        ("FETCH", None),
        ("IXSCAN", None),
    ]
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog("RM-576")
def test_a_rooted_ors_plan_summary_names_each_branchs_index(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's profile entry for `{$or: [{a: {$gte: 2}, b: {$gte:
    3}}, {a: 0, b: 0}]}` over `a_1` and `b_1` names the index each branch
    won with: `IXSCAN { a: 1 }, IXSCAN { b: 1 }`. This server's explain
    names the same two, but its profile entry says `IXSCAN { a: 1 }, IXSCAN
    { a: 1 }`."""

    def run(client: Any) -> Any:
        db = client["rm_found_471_" + uuid.uuid4().hex[:8]]
        try:
            db.c.insert_many([{"_id": n, "a": n % 7, "b": n % 11} for n in range(600)])
            db.c.create_index("a")
            db.c.create_index("b")
            db.command("profile", 2)
            list(db.c.find({"$or": [{"a": {"$gte": 2}, "b": {"$gte": 3}}, {"a": 0, "b": 0}]}))
            db.command("profile", 0)
            entry = db.system.profile.find_one({"op": "query", "ns": f"{db.name}.c"})
            return entry and entry.get("planSummary")
        finally:
            client.drop_database(db.name)

    want = run(mongod)
    assert want == "IXSCAN { a: 1 }, IXSCAN { b: 1 }"
    assert run(rust_mongo) == want
