"""Gap tests for the parts of a `$lookup` mongod 8.2.11 runs in SBE that
this server does not yet explain as SBE (RM-683 slice 1 built the hash join
over a plain query; what follows is what working it found and left),
compared with mongod 8.2.11 (a one-member replica set, macOS arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture


def _join(**over: Any) -> dict:
    spec = {"from": "d", "localField": "a", "foreignField": "b", "as": "z"}
    spec.update(over)
    return {"$lookup": spec}


def _top(
    client: Any, pipeline: list, foreign: list, index: Any = None, local_index: Any = None
) -> Any:
    """How the reply names the `$lookup` it explains: its explain version,
    whether it lists classic `stages`, and the strategy of its `EQ_LOOKUP`."""
    client.drop_database("rm683lk")
    db = client.rm683lk
    db.c.insert_many([{"_id": n, "a": n} for n in range(1, 4)])
    db.d.insert_many(foreign)
    if index:
        db.d.create_index(index)
    if local_index:
        db.c.create_index(local_index)

    def run() -> dict:
        reply = db.command(
            "explain",
            {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            verbosity="queryPlanner",
        )
        plan = reply.get("queryPlanner", {}).get("winningPlan", {}).get("queryPlan", {})
        while plan.get("stage") not in (None, "EQ_LOOKUP"):
            plan = plan.get("inputStage", {})
        return {
            "explainVersion": reply.get("explainVersion"),
            "stages": "stages" in reply,
            "strategy": plan.get("strategy"),
            "slotBased": "slotBasedPlan" in reply.get("queryPlanner", {}).get("winningPlan", {}),
        }

    return mongo_answer(run)


_FOREIGN = [{"_id": n, "b": n} for n in range(1, 3)]


def _indexed(client: Any) -> dict:
    return {
        "index on the foreign field": _top(client, [_join()], _FOREIGN, [("b", 1)]),
        "compound index led by it": _top(client, [_join()], _FOREIGN, [("b", 1), ("x", 1)]),
        "foreign _id": _top(client, [_join(foreignField="_id")], _FOREIGN),
    }


@pytest.mark.backlog("RM-683")
def test_a_lookup_on_an_indexed_foreign_field_is_an_indexed_loop_join(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 explains a `$lookup` whose foreign field leads an index
    (and one on `_id`) as SBE with the `IndexedLoopJoin` strategy; this
    server explains it as a classic `$cursor` and `$lookup`."""
    assert _indexed(rust_mongo) == _indexed(mongod_rs)


def _nested(client: Any) -> dict:
    big = [{"_id": n, "b": n % 7} for n in range(10_001)]
    return {"past 10,000 documents": _top(client, [_join()], big)}


@pytest.mark.backlog("RM-683")
def test_a_lookup_on_a_large_foreign_collection_is_a_nested_loop_join(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 joins a foreign collection of more than 10,000
    documents (or 100 MiB) by `NestedLoopJoin`, explained as SBE; this
    server explains it as a classic `$cursor` and `$lookup`."""
    assert _nested(rust_mongo) == _nested(mongod_rs)


def _shapes(client: Any) -> dict:
    pipelines = {
        "project after": [_join(), {"$project": {"z": 1}}],
        "limit after": [_join(), {"$limit": 1}],
        "skip after": [_join(), {"$skip": 1}],
        "sort after": [_join(), {"$sort": {"a": 1}}],
        "match after on another field": [_join(), {"$match": {"a": 1}}],
        "group after": [_join(), {"$group": {"_id": "$a"}}],
        "count after": [_join(), {"$count": "n"}],
        "sort before": [{"$sort": {"a": 1}}, _join()],
        "project before": [{"$project": {"a": 1}}, _join()],
        "group before": [{"$group": {"_id": "$a"}}, _join(localField="_id")],
        "as a dotted path": [_join(**{"as": "z.y"})],
    }
    shapes = {name: _top(client, p, _FOREIGN) for name, p in pipelines.items()}
    shapes["match before on an indexed field"] = _top(
        client, [{"$match": {"a": {"$gt": 1}}}, _join()], _FOREIGN, local_index=[("a", 1)]
    )
    return shapes


@pytest.mark.backlog("RM-827")
def test_a_lookup_among_other_pushed_stages_is_explained_as_sbe(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 runs a `$lookup` in SBE beside a `$sort`, `$project`
    or `$group` before it, a `$project`, `$group`, `$count`, `$limit`,
    `$skip` or `$sort` after it (the last three move ahead of it into the
    query; a `$project` after it puts a projection of the fields it reads
    under it), over a query an index serves, and with a dotted `as`; this
    server explains each as a classic `$cursor` and `$lookup`."""
    assert _shapes(rust_mongo) == _shapes(mongod_rs)


def _classic(client: Any, pipeline: list) -> Any:
    """The stages a classic explain lists after the cursor, and the cursor's
    parsed query."""
    client.drop_database("rm683lk")
    db = client.rm683lk
    db.c.insert_many([{"_id": n, "a": n} for n in range(1, 4)])
    db.d.insert_many(_FOREIGN)

    def run() -> dict:
        reply = db.command(
            "explain",
            {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            verbosity="queryPlanner",
        )
        stages = reply.get("stages", [])
        cursor = stages[0].get("$cursor", {}).get("queryPlanner", {}) if stages else {}
        return {
            "parsedQuery": cursor.get("parsedQuery"),
            "stages": [
                {k: v for k, v in s.items() if k not in ("$cursor", "nReturned")}
                for s in stages[1:]
            ],
        }

    return mongo_answer(run)


def _swapped(client: Any) -> dict:
    # A `$lookup` with a pipeline stays classic.
    classic = {"$lookup": {"from": "d", "let": {}, "pipeline": [], "as": "z"}}
    return {
        "match on another field": _classic(client, [classic, {"$match": {"a": 1}}]),
        "match on the joined field": _classic(client, [classic, {"$match": {"z.b": 1}}]),
    }


@pytest.mark.backlog("RM-828")
def test_a_match_moves_ahead_of_a_classic_lookup_that_does_not_read_it(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 moves a `$match` that does not read the `as` field
    ahead of the `$lookup`, into the query (`parsedQuery` {a: {$eq: 1}});
    this server lists it after the `$lookup` with an empty query. Found
    2026-11-26 working RM-683."""
    assert _swapped(rust_mongo) == _swapped(mongod_rs)


def _unwound(client: Any) -> dict:
    classic = {"$lookup": {"from": "d", "let": {}, "pipeline": [], "as": "z"}}
    return {
        "on join fields": _classic(client, [_join(), {"$unwind": "$z"}]),
        "with a pipeline": _classic(client, [classic, {"$unwind": "$z"}]),
        "preserving": _classic(
            client, [_join(), {"$unwind": {"path": "$z", "preserveNullAndEmptyArrays": True}}]
        ),
    }


@pytest.mark.backlog("RM-829")
def test_an_unwind_after_a_lookup_is_listed_inside_it(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 absorbs a `$unwind` of the `as` field into the
    `$lookup` stage it lists (`unwinding: {preserveNullAndEmptyArrays:
    false}`); this server lists the two stages. Found 2026-11-26 working
    RM-683."""
    assert _unwound(rust_mongo) == _unwound(mongod_rs)
