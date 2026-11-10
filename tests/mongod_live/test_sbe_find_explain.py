"""`explain` of a `find` a query setting puts in mongod's slot-based engine
(RM-418).

Under a `queryFramework: "sbe"` query setting mongod 8.2.11 runs a `find`
in SBE (`trySbeEngine`) and explains it with `explainVersion` "2": a
`winningPlan` of `{queryPlan, slotBasedPlan}` and `executionStats` made of
the SBE stages (`scan`, `filter`, `limit` / `limitskip`) that ran it. Its
`planCacheKey` hashes the settings, and `serverParameters` names the
framework control the setting chose. Under `"classic"` (`forceClassicEngine`)
a `$group` mongod would push into SBE stays classic. The whole reply is
compared, slot program text included, with each server's collection UUID
replaced by a placeholder; timings are not compared, nor the classic work
counters RM-434 is about.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import same

DB = "live_sbe_find"
VOLATILE = {
    "executionTimeMillisEstimate",
    "executionTimeMillis",
    "optimizationTimeMillis",
    "serverInfo",
    "command",
    "$clusterTime",
    "operationTime",
    # The classic stages' work accounting (RM-434).
    "works",
    "advanced",
    "needTime",
    "needYield",
    "saveState",
    "restoreState",
    "isEOF",
}
# The SBE stages count their own `saveState` / `restoreState` / `isEOF`:
# compared there.
SBE_KEPT = {"saveState", "restoreState", "isEOF"}

DOCS = [{"x": i, "y": i % 2, "s": str(i)} for i in range(6)]

FINDS = [
    {"filter": {}},
    {},
    {"filter": {"x": 1}},
    {"filter": {"x": {"$gt": 1}, "y": 1}},
    {"filter": {"s": {"$lte": "3"}}},
    {"filter": {"x": {"$gt": 0}}, "skip": 1},
    {"filter": {"x": {"$gt": 0}}, "limit": 2},
    {"filter": {"x": {"$gt": 0}}, "skip": 1, "limit": 2},
    {"filter": {"x": {"$gt": 0}}, "limit": 20},
]


def strip(x: Any, uuid: str, sbe: bool = False) -> Any:
    if isinstance(x, dict):
        sbe = sbe or "slotBasedPlan" in x or "planNodeId" in x
        return {
            k: strip(v, uuid, sbe)
            for k, v in x.items()
            if k not in VOLATILE or (sbe and k in SBE_KEPT)
        }
    if isinstance(x, list):
        return [strip(v, uuid, sbe) for v in x]
    if isinstance(x, str):
        return x.replace(uuid, "<uuid>")
    return x


def _clear(client: Any) -> None:
    for e in list(client.admin.aggregate([{"$querySettings": {}}])):
        client.admin.command({"removeQuerySettings": e["queryShapeHash"]})


def _explain(client: Any, coll: str, docs: list, cmd: dict, framework: str, verbosity: str) -> Any:
    db = client[DB]
    if docs and coll not in db.list_collection_names():
        db[coll].insert_many([dict(d) for d in docs])
    _clear(client)
    client.admin.command(
        {"setQuerySettings": dict(cmd, **{"$db": DB}), "settings": {"queryFramework": framework}}
    )
    try:
        uuid = "<none>"
        for c in db.list_collections():
            if c["name"] == coll:
                raw = c["info"]["uuid"]
                uuid = str(raw.as_uuid() if hasattr(raw, "as_uuid") else raw)
        return strip(db.command("explain", cmd, verbosity=verbosity), uuid)
    finally:
        _clear(client)


@pytest.mark.parametrize("verbosity", ["queryPlanner", "executionStats", "allPlansExecution"])
@pytest.mark.parametrize("find", FINDS, ids=[str(f) for f in FINDS])
def test_sbe_find_explain_matches_mongod(
    rust_mongo: Any, mongod_rs: Any, find: dict, verbosity: str
) -> None:
    cmd = dict({"find": "c"}, **find)
    same(
        _explain(rust_mongo, "c", DOCS, cmd, "sbe", verbosity),
        _explain(mongod_rs, "c", DOCS, cmd, "sbe", verbosity),
    )


def test_sbe_find_of_a_missing_collection(rust_mongo: Any, mongod_rs: Any) -> None:
    """The `limit 0` / `coscan` plan mongod builds over no collection."""
    cmd = {"find": "missing", "filter": {"x": 1}}
    same(
        _explain(rust_mongo, "missing", [], cmd, "sbe", "executionStats"),
        _explain(mongod_rs, "missing", [], cmd, "sbe", "executionStats"),
    )


@pytest.mark.parametrize(
    "cmd",
    [
        {"find": "c", "filter": {"x": 1}},
        {"aggregate": "c", "pipeline": [{"$group": {"_id": "$y"}}], "cursor": {}},
    ],
    ids=["find", "group"],
)
def test_classic_framework_explain_matches_mongod(
    rust_mongo: Any, mongod_rs: Any, cmd: dict
) -> None:
    """`"classic"` keeps a `$group` out of SBE, and a `find`'s cache key
    and framework control say so."""
    same(
        _explain(rust_mongo, "c", DOCS, cmd, "classic", "queryPlanner"),
        _explain(mongod_rs, "c", DOCS, cmd, "classic", "queryPlanner"),
    )


def _profiled(client: Any, framework: str) -> list:
    db = client[DB]
    db.p.drop()
    db.p.insert_many([{"x": i} for i in range(10)])
    cmd = {"find": "p", "filter": {"x": {"$gt": 1}}}
    _clear(client)
    client.admin.command(
        {"setQuerySettings": dict(cmd, **{"$db": DB}), "settings": {"queryFramework": framework}}
    )
    try:
        db.system.profile.drop()
        db.command("profile", 2)
        list(db.p.find({"x": {"$gt": 1}}).batch_size(3))
        db.command("profile", 0)
        return [
            {k: e.get(k) for k in ("op", "queryFramework", "planCacheShapeHash", "planCacheKey")}
            for e in db.system.profile.find({"ns": f"{DB}.p", "op": {"$in": ["query", "getmore"]}})
        ]
    finally:
        _clear(client)


@pytest.mark.parametrize("framework", ["sbe", "classic"])
def test_profile_entries_name_the_engine(rust_mongo: Any, mongod_rs: Any, framework: str) -> None:
    """The `find` and each `getMore` report the engine the setting chose,
    and the cache key the setting moved."""
    same(_profiled(rust_mongo, framework), _profiled(mongod_rs, framework))
