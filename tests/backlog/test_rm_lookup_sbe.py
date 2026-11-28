"""Gap tests for the parts of a `$lookup` mongod 8.2.11 runs in SBE that
this server does not yet explain as SBE (RM-683 slice 1 built the hash join
over a plain query; what follows is what working it found and left),
compared with mongod 8.2.11 (a one-member replica set, macOS arm64).
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import Decimal128

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture


def _join(**over: Any) -> dict:
    spec = {"from": "d", "localField": "a", "foreignField": "b", "as": "z"}
    spec.update(over)
    return {"$lookup": spec}


def _top(
    client: Any,
    pipeline: list,
    foreign: list,
    index: Any = None,
    local_index: Any = None,
    options: dict | None = None,
    index_options: dict | None = None,
    local: list | None = None,
    local_collection: dict | None = None,
) -> Any:
    """How the reply names the `$lookup` it explains: its explain version,
    whether it lists classic `stages`, and the strategy of its `EQ_LOOKUP`."""
    client.drop_database("rm683lk")
    db = client.rm683lk
    if local_collection:
        db.create_collection("c", **local_collection)
    db.c.insert_many(local or [{"_id": n, "a": n} for n in range(1, 4)])
    db.d.insert_many(foreign)
    if index:
        db.d.create_index(index, **(index_options or {}))
    if local_index:
        db.c.create_index(local_index)

    def run() -> dict:
        reply = db.command(
            "explain",
            {"aggregate": "c", "pipeline": pipeline, "cursor": {}, **(options or {})},
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


@pytest.mark.backlog_fixed("RM-683")
def test_a_lookup_on_an_indexed_foreign_field_is_an_indexed_loop_join(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 explains a `$lookup` whose foreign field leads an index
    (and one on `_id`) as SBE with the `IndexedLoopJoin` strategy; this
    server explains it as a classic `$cursor` and `$lookup`."""
    assert _indexed(rust_mongo) == _indexed(mongod_rs)


def _decimal(client: Any) -> dict:
    local = [{"_id": 1, "a": Decimal128("4")}, {"_id": 2, "a": [Decimal128("1.5"), 2]}]
    foreign = [{"_id": 1, "b": 4}, {"_id": 2, "b": Decimal128("1.5")}]
    return {"a decimal local key": _top(client, [_join()], foreign, [("b", 1)], local=local)}


@pytest.mark.backlog("RM-838")
def test_an_indexed_loop_join_over_decimal_keys_is_explained_as_sbe(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 seeks a decimal as it seeks any number (a decimal and
    an int or double that are equal share a KeyString); this server's
    KeyString encoder does not write a `Decimal128`, so it explains the
    `$lookup` as a classic `$cursor` and `$lookup`."""
    assert _decimal(rust_mongo) == _decimal(mongod_rs)


def _collated(client: Any) -> dict:
    en = {"collation": {"locale": "en", "strength": 2}}
    return {
        "a command collation over a collated index": _top(
            client, [_join()], _FOREIGN, [("b", 1)], options=en, index_options=en
        ),
        "a command collation over a plain index": _top(
            client, [_join()], _FOREIGN, [("b", 1)], options=en
        ),
        "a collated local collection": _top(
            client, [_join()], _FOREIGN, [("b", 1)], local_collection=en
        ),
        "a collated foreign index under a plain query": _top(
            client, [_join()], _FOREIGN, [("b", 1)], index_options=en
        ),
    }


@pytest.mark.backlog("RM-839")
def test_a_lookup_under_a_collation_is_explained_as_sbe(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 explains a `$lookup` under the command's collation, or
    over a local collection with a default one, as SBE, joining by an index
    whose collation is the query's (`IndexedLoopJoin`, its seeks made with
    `collKs`) and by hash or dynamic indexed loop otherwise; this server
    explains each as a classic `$cursor` and `$lookup`."""
    assert _collated(rust_mongo) == _collated(mongod_rs)


def _dynamic(client: Any) -> dict:
    en = {"collation": {"locale": "en"}}
    return {
        "without allowDiskUse": _top(
            client,
            [_join()],
            _FOREIGN,
            [("b", 1)],
            options={"allowDiskUse": False},
            index_options=en,
        ),
        "past 10,000 documents": _top(
            client,
            [_join()],
            [{"_id": n, "b": n} for n in range(10_001)],
            [("b", 1)],
            index_options=en,
        ),
    }


@pytest.mark.backlog("RM-840")
def test_a_collated_index_without_a_hash_join_is_a_dynamic_indexed_loop_join(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 joins by `DynamicIndexedLoopJoin` when the only index
    that leads with the foreign field has a collation the query lacks and no
    hash join is possible (`allowDiskUse: false`, or a foreign collection
    past 10,000 documents), explained as SBE; this server explains it as a
    classic `$cursor` and `$lookup`."""
    assert _dynamic(rust_mongo) == _dynamic(mongod_rs)


def _nested(client: Any) -> dict:
    big = [{"_id": n, "b": n % 7} for n in range(10_001)]
    return {
        "past 10,000 documents": _top(client, [_join()], big),
        "without allowDiskUse": _top(client, [_join()], _FOREIGN, options={"allowDiskUse": False}),
    }


@pytest.mark.backlog_fixed("RM-683")
def test_a_lookup_on_a_large_foreign_collection_is_a_nested_loop_join(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 joins a foreign collection of more than 10,000
    documents (or 100 MiB), or any collection no index serves when the
    command has `allowDiskUse: false` (a hash join needs it), by
    `NestedLoopJoin`, explained as SBE; this server explains it as a classic
    `$cursor` and `$lookup`."""
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
        "with an array index": _classic(
            client, [_join(), {"$unwind": {"path": "$z", "includeArrayIndex": "i"}}]
        ),
    }


@pytest.mark.backlog_fixed("RM-829")
def test_an_unwind_after_a_lookup_is_listed_inside_it(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 absorbs a `$unwind` of the `as` field into the
    `$lookup` stage it lists (`unwinding: {preserveNullAndEmptyArrays:
    false}`); this server listed the two stages until RM-829 folded them.
    Found 2026-11-26 working RM-683."""
    assert _unwound(rust_mongo) == _unwound(mongod_rs)
