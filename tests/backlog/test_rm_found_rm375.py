"""Divergences found while working RM-375 (2026-11-09), each measured
against mongod 8.2.11, and the regression test of what RM-375 fixed."""

from __future__ import annotations

from typing import Any

import bson
import pytest

from tests.backlog.opdebug import logged

_KEYS = [("y", 1), ("x", 1)]


def _sort_stage(stats: Any) -> dict:
    while isinstance(stats, dict):
        if stats.get("stage") == "SORT":
            return stats
        stats = stats.get("inputStage")
    raise AssertionError("no SORT stage")


def _profiled_sort(
    client: Any, is_mongod: bool, docs: list, keys: list, command: dict
) -> tuple[Any, Any]:
    """The `type` and `totalDataSizeSorted` of the SORT in `command`'s
    profile entry, over `docs` and an index on `keys`."""

    def setup(d: Any) -> None:
        d.c.insert_many(docs)
        d.c.create_index(keys)

    entries = logged(client, is_mongod, setup, lambda d: d.command({"find": "c", **command}))[1]
    stage = _sort_stage(entries[0].get("execStats"))
    return stage.get("type"), stage.get("totalDataSizeSorted")


def _ints(n: int = 12) -> list:
    return [{"_id": i, "x": i, "y": i % 3} for i in range(n)]


_ON_KEYS = {"filter": {"y": {"$gte": 0}}, "sort": {"x": 1}}

#: Shapes whose SORT of index keys counted nothing before RM-375, with
#: mongod 8.2.11's count: each member's sort key, its index key and 32.
_SHAPES: dict[str, tuple[list, list, dict, int]] = {
    "int": (_ints(), _KEYS, _ON_KEYS, 780),
    "compound sort": (_ints(), _KEYS, {**_ON_KEYS, "sort": {"x": 1, "y": -1}}, 1644),
    "three-field key": (
        [{"_id": i, "x": i, "y": i % 3, "z": i % 2} for i in range(12)],
        [("y", 1), ("x", 1), ("z", 1)],
        _ON_KEYS,
        852,
    ),
    "string": (
        [{"_id": i, "x": f"s{'s' * 19}{i}", "y": i % 3} for i in range(12)],
        _KEYS,
        _ON_KEYS,
        1588,
    ),
    "double": ([{"_id": i, "x": float(i), "y": i % 3} for i in range(12)], _KEYS, _ON_KEYS, 828),
    "decimal": (
        [{"_id": i, "x": bson.Decimal128(str(i)), "y": i % 3} for i in range(12)],
        _KEYS,
        _ON_KEYS,
        1308,
    ),
    "document": ([{"_id": i, "x": {"k": i}, "y": i % 3} for i in range(12)], _KEYS, _ON_KEYS, 2268),
    "missing": ([{"_id": i, "y": i % 3} for i in range(12)], _KEYS, _ON_KEYS, 732),
    "limit 3": (_ints(), _KEYS, {**_ON_KEYS, "limit": 3}, 195),
    "limit 1": (_ints(), _KEYS, {**_ON_KEYS, "limit": 1}, 0),
    "collation": (_ints(), _KEYS, {**_ON_KEYS, "collation": {"locale": "fr"}}, 780),
}


@pytest.mark.backlog_fixed("RM-375")
def test_a_sort_of_index_keys_counts_each_member_as_mongod_does(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's SORT of index keys counts each working-set member as
    its sort key, the index key it was found at and 32 bytes, over the
    keys' types, the sort's arity and its limit. This server's counted
    nothing (RM-375)."""
    answers = {
        side: {
            name: _profiled_sort(client, is_mongod, docs, keys, command)
            for name, (docs, keys, command, _) in _SHAPES.items()
        }
        for side, client, is_mongod in (("mongod", mongod, True), ("rust", rust_mongo, False))
    }
    assert answers["mongod"] == {name: ("default", n) for name, (*_, n) in _SHAPES.items()}
    assert answers["rust"] == answers["mongod"]


def _budget_answers(client: Any) -> dict:
    db = client["rm_found_rm375_budget"]
    db.drop_collection("c")
    db.c.insert_many(_ints())
    db.c.create_index(_KEYS)
    out = {}
    try:
        for budget, limit in ((915, 0), (916, 0), (330, 3), (331, 3)):
            client.admin.command(
                "setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=budget
            )
            try:
                limited = {"limit": limit} if limit else {}
                db.command("find", "c", allowDiskUse=False, **_ON_KEYS, **limited)
                out[(budget, limit)] = "ok"
            except Exception as e:  # noqa: BLE001 - the answer is the error
                out[(budget, limit)] = getattr(e, "code", repr(e))
    finally:
        client.admin.command(
            "setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=104857600
        )
        client.drop_database("rm_found_rm375_budget")
    return out


@pytest.mark.backlog_fixed("RM-375")
def test_a_sort_of_index_keys_is_held_to_its_budget(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses (292) a SORT of index keys whose members pass
    its budget less one 136-byte file iterator without `allowDiskUse`:
    twelve 65-byte members need 916, three kept by a limit 331. This
    server's sort of keys was held to no budget (found with RM-375)."""
    answers = {"mongod": _budget_answers(mongod), "rust": _budget_answers(rust_mongo)}
    assert answers["mongod"] == {(915, 0): 292, (916, 0): "ok", (330, 3): 292, (331, 3): "ok"}
    assert answers["rust"] == answers["mongod"]


def _spilled(client: Any, is_mongod: bool) -> dict:
    client.admin.command("setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=100000)
    try:

        def setup(d: Any) -> None:
            d.c.insert_many(_ints(2000))
            d.c.create_index(_KEYS)

        def op(d: Any) -> None:
            d.command("find", "c", allowDiskUse=True, batchSize=3000, **_ON_KEYS)

        entry = logged(client, is_mongod, setup, op)[1][0]
    finally:
        client.admin.command(
            "setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=104857600
        )
    stage = _sort_stage(entry.get("execStats"))
    return {
        "entry usedDisk": entry.get("usedDisk"),
        "sortSpills": entry.get("sortSpills"),
        **{k: stage.get(k) for k in ("usedDisk", "spills", "spilledRecords", "spilledBytes")},
    }


@pytest.mark.backlog("RM-475")
def test_a_sort_of_index_keys_past_its_budget_spills(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SORT of index keys spills past its budget with
    `allowDiskUse`: 2000 65-byte members under a 100000-byte budget spill
    twice, every record, 130000 bytes, and the profile entry says
    `usedDisk` and `sortSpills: 2`. This server's counts the members but
    never spills, and reports no disk used."""
    answers = {"mongod": _spilled(mongod, True), "rust": _spilled(rust_mongo, False)}
    assert answers["mongod"] == {
        "entry usedDisk": True,
        "sortSpills": 2,
        "usedDisk": True,
        "spills": 2,
        "spilledRecords": 2000,
        "spilledBytes": 130000,
    }
    assert answers["rust"] == answers["mongod"]


def _explained(client: Any) -> Any:
    db = client["rm_found_rm375_explain"]
    db.drop_collection("c")
    db.c.insert_many([{"_id": i, "x": i, "y": i % 3, "pad": "p" * 50} for i in range(12)])
    db.c.create_index(_KEYS)
    e = db.command("explain", {"find": "c", **_ON_KEYS}, verbosity="executionStats")
    client.drop_database("rm_found_rm375_explain")
    stage = _sort_stage(e["executionStats"]["executionStages"])
    return stage.get("type"), stage.get("totalDataSizeSorted")


def _sorts(stats: Any) -> list:
    """The `type` and `totalDataSizeSorted` of each SORT in a plan's stats."""
    out = []
    while isinstance(stats, dict):
        if stats.get("stage") == "SORT":
            out.append((stats.get("type"), stats.get("totalDataSizeSorted")))
        stats = stats.get("inputStage")
    return out


def _explained_shapes(client: Any) -> dict:
    """Each of `_SHAPES`' SORTs as `explain` counts it: in `executionStats`,
    and in `allPlansExecution` beside a competing `{x: 1}` index, whose
    trial runs the SORT plan too."""
    db = client["rm_found_rm375_shapes"]
    out = {}
    try:
        for name, (docs, keys, command, _) in _SHAPES.items():
            db.drop_collection("c")
            db.c.insert_many(docs)
            db.c.create_index(keys)
            explain = {"find": "c", **command}
            e = db.command("explain", explain, verbosity="executionStats")
            out[name] = _sorts(e["executionStats"]["executionStages"])
            db.c.create_index([("x", 1)])
            e = db.command("explain", explain, verbosity="allPlansExecution")
            out[f"{name}, trial"] = [
                s
                for plan in e["executionStats"]["allPlansExecution"]
                for s in _sorts(plan.get("executionStages"))
            ]
    finally:
        client.drop_database("rm_found_rm375_shapes")
    return out


@pytest.mark.backlog_fixed("RM-476")
def test_an_explained_sort_of_index_keys_counts_its_members(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `explain` of a SORT of index keys counts its members
    as its execution does: 780 for twelve `{y, x}` keys, whatever the
    documents hold, and each of `_SHAPES` as its profile entry counts it,
    in `executionStats` and in an `allPlansExecution` trial. This server's
    `explain` sized the documents behind the keys, as a sort of documents
    counts (1440 with a 50-byte pad), until RM-476."""
    answers = {"mongod": _explained(mongod), "rust": _explained(rust_mongo)}
    assert answers["mongod"] == ("default", 780)
    assert answers["rust"] == answers["mongod"]
    shapes = {"mongod": _explained_shapes(mongod), "rust": _explained_shapes(rust_mongo)}
    # The trial counts a limit's SORT before it has sorted: none.
    assert shapes["mongod"]["limit 3"] == [("default", 195)]
    assert shapes["mongod"]["limit 3, trial"] == [("default", 0)]
    assert shapes["mongod"]["string"] == [("default", 1588)]
    assert shapes["rust"] == shapes["mongod"]


@pytest.mark.backlog("RM-477")
def test_a_sort_beside_a_multikey_field_sorts_index_keys(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sorts index keys (`type: "default"`, below the FETCH)
    for `find({y: {$gte: 0}}).sort({x: 1})` over `{y: 1, x: 1}` when `y`
    is multikey and `x` is not: 780 for twelve members. This server sorts
    the fetched documents above the FETCH (`simple`, 900)."""
    docs = [{"_id": i, "x": i, "y": [i % 3, 5]} for i in range(12)]
    answers = {
        side: _profiled_sort(client, is_mongod, docs, _KEYS, _ON_KEYS)
        for side, client, is_mongod in (("mongod", mongod, True), ("rust", rust_mongo, False))
    }
    assert answers["mongod"] == ("default", 780)
    assert answers["rust"] == answers["mongod"]
