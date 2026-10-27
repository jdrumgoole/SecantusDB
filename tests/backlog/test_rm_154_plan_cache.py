"""mongod's plan cache (RM-154, RM-215 part), against mongod 8.2.11 live.

mongod runs its trial of candidate plans once for a query shape and caches
the winner: an inactive entry first, active once a later trial takes no
more works, reused while it does no worse than ten times those works and
replanned when it does, cleared when the collection's indexes change or by
`planCacheClear`, an LRU of 5000 entries per collection. The cached plan
decides which plan -- and so which order -- a repeated shape gets, and
`explain` and `$planCacheStats` report it. Each test runs one scenario on
both servers, on a collection of its own, and compares what the scenario
reduces to.

Not compared: `timeOfCreation` and `host` (this run's), `solutionHash`
(mongod's is `absl::Hash`, seeded per process: two runs of one mongod do
not agree), timings and yields (`saveState`, `restoreState`: mongod yields
when 10 ms have passed), and `estimatedSizeBytes` (RM-341).
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest
from pymongo.errors import PyMongoError

VOLATILE = {
    "timeOfCreation",
    "host",
    "solutionHash",
    "estimatedSizeBytes",
    "executionTimeMillisEstimate",
    "saveState",
    "restoreState",
    "$clusterTime",
    "operationTime",
}


def answer(fn: Callable[[], Any]) -> Any:
    try:
        return fn()
    except PyMongoError as e:
        return ("err", getattr(e, "code", None), str(getattr(e, "details", {}).get("errmsg")))


def strip(x: Any) -> Any:
    if isinstance(x, dict):
        return {k: strip(v) for k, v in x.items() if k not in VOLATILE}
    if isinstance(x, list):
        return [strip(v) for v in x]
    return x


def fresh(client: Any, db: str, n: int = 2000, gen: Any = None, indexes: Any = ("a", "b")) -> Any:
    client.drop_database(db)
    col = client[db].c
    gen = gen or (lambda i: {"_id": i, "a": i % 100, "b": (i // 20) % 100})
    col.insert_many([gen(i) for i in range(n)])
    for ix in indexes:
        col.create_index(ix)
    return col


def ok(fn: Callable[[], Any]) -> Any:
    """`"ok"` for a command that succeeded (its reply's cluster time is a
    replica set's), else the error."""
    got = answer(fn)
    return got[:2] if isinstance(got, tuple) else "ok"


def stats(col: Any) -> Any:
    return answer(lambda: [strip(e) for e in col.aggregate([{"$planCacheStats": {}}])])


def summary(col: Any) -> Any:
    got = stats(col)
    if not isinstance(got, list):
        return got
    return [
        (e["planCacheShapeHash"], e["planCacheKey"], e["isActive"], e["works"], e["worksType"])
        for e in got
    ]


# ------------------------------------------- a repeated shape, its order --


def _repeated_shape(client: Any) -> list:
    """mongod 8.2.11: `{a: {$lt: x}, b: {$lt: y}}` over `a_1` and `b_1`.
    The first trial picks `a_1` (inactive, 101 works); the second picks
    `b_1` with 202 works, so the entry stays inactive at 202; the third
    takes 101 and activates `b_1`, which the next two reuse -- returning
    their documents in `b_1`'s order where a trial would pick `a_1`. Then
    `b_1` runs past ten times its works, is replanned, and `a_1` is cached
    active at 61 works; and back again."""
    col = fresh(client, "rm154order")
    out = []
    for x, y in [(5, 90), (30, 20), (90, 5), (50, 50), (20, 30), (3, 60), (60, 3), (40, 40)]:
        ids = [d["_id"] for d in col.find({"a": {"$lt": x}, "b": {"$lt": y}})]
        entries = answer(lambda: list(col.aggregate([{"$planCacheStats": {}}])))
        if isinstance(entries, list):
            entries = [
                (e["isActive"], e["works"], e["cachedPlan"]["inputStage"].get("indexName"))
                for e in entries
            ]
        out.append((x, y, ids, entries))
    return out


@pytest.mark.backlog_fixed("RM-154")
def test_a_repeated_shape_runs_the_cached_plan(rust_mongo: Any, mongod: Any) -> None:
    assert _repeated_shape(rust_mongo) == _repeated_shape(mongod)


# ------------------------------------------------- what explain reports --


def _explained(client: Any) -> list:
    col = fresh(
        client,
        "rm154explain",
        n=50,
        gen=lambda i: {"_id": i, "a": i % 10, "b": i % 7, "u": i},
        indexes=("a", "b"),
    )
    col.create_index("u", unique=True)
    db = col.database
    cases = [
        {"find": "c", "filter": {"a": 1, "b": 2}},
        {"find": "c", "filter": {"_id": 3}},
        {"find": "c", "filter": {"u": 3}},
        {"find": "c", "filter": {}},
        {"find": "nope", "filter": {"a": 1}},
        {"find": "c", "filter": {"a": 1}, "hint": {"a": 1}},
        {"find": "c", "filter": {"a": 1}, "sort": {"b": 1}, "projection": {"a": 1}},
        {"find": "c", "filter": {"a": {"$in": [1, 2]}, "b": {"$ne": None}}},
        {"find": "c", "filter": {"$or": [{"a": 1}, {"b": {"$gt": 2}}]}},
        {"find": "c", "filter": {"a": {"$not": {"$gt": 3}}}},
        {"find": "c", "filter": {"a": 1}, "collation": {"locale": "fr", "strength": 2}},
        {"count": "c"},
        {"count": "c", "query": {"a": 1}},
        {"distinct": "c", "key": "b", "query": {"a": 1}},
        {"update": "c", "updates": [{"q": {"a": 1, "b": 2}, "u": {"$set": {"z": 1}}}]},
        {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$set": {"z": 1}}}]},
        {"delete": "c", "deletes": [{"q": {"a": 1}, "limit": 1}]},
        {"findAndModify": "c", "query": {"a": 1}, "update": {"$set": {"z": 1}}},
        {"aggregate": "c", "pipeline": [{"$match": {"a": 1}}], "cursor": {}},
        {
            "aggregate": "c",
            "pipeline": [{"$match": {"a": 1}}, {"$group": {"_id": "$b"}}],
            "cursor": {},
        },
    ]
    out = []
    for cmd in cases:
        e = answer(lambda cmd=cmd: db.command("explain", cmd, verbosity="queryPlanner"))
        if isinstance(e, tuple):
            out.append(e)
            continue
        qp = e.get("queryPlanner") or e["stages"][0]["$cursor"]["queryPlanner"]
        out.append(
            {k: qp.get(k) for k in ("queryHash", "planCacheShapeHash", "planCacheKey") if k in qp}
        )
    # `isCached`: the winning plan an active entry holds, for the query of
    # the values it was cached with.
    q = {"a": {"$lt": 5}, "b": {"$lt": 3}}
    cached = []
    for _ in range(3):
        list(col.find(q))
        e = db.command("explain", {"find": "c", "filter": q}, verbosity="allPlansExecution")
        cached.append(
            (
                e["queryPlanner"]["winningPlan"]["isCached"],
                [p["isCached"] for p in e["queryPlanner"]["rejectedPlans"]],
                [
                    p["executionStages"]["isCached"]
                    for p in e["executionStats"]["allPlansExecution"]
                ],
            )
        )
    other = {"a": {"$lt": 6}, "b": {"$lt": 3}}
    e = db.command("explain", {"find": "c", "filter": other}, verbosity="queryPlanner")
    cached.append(e["queryPlanner"]["winningPlan"]["isCached"])
    out.append(cached)
    return out


@pytest.mark.backlog_fixed("RM-154")
def test_explain_reports_the_plan_cache(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 prints `queryHash`, `planCacheShapeHash` and
    `planCacheKey` -- MurmurHash3 of its query shape and key, byte for byte
    -- in every explain but a missing collection's, a fast count's and an
    express write's, and `isCached` on the plan an active entry holds."""
    assert _explained(rust_mongo) == _explained(mongod)


# --------------------------------------------- what $planCacheStats lists --


def _entries(client: Any) -> list:
    col = fresh(
        client,
        "rm154stats",
        gen=lambda i: {"_id": i, "a": i % 100, "b": (i // 20) % 100, "t": [i % 3, i % 5]},
        indexes=("a", "b", [("a", 1), ("b", 1)], "t"),
    )
    queries = [
        ({"a": {"$lt": 5}, "b": {"$lt": 90}}, None, None, 0, 0),
        ({"a": 1, "b": 1}, None, None, 0, 0),
        ({"a": {"$gte": 3}, "b": 2}, [("a", 1)], None, 0, 0),
        ({"a": {"$lt": 50}, "b": {"$lt": 50}}, None, {"a": 1, "_id": 0}, 0, 7),
        ({"a": {"$lt": 50}, "b": {"$lt": 50}}, None, None, 3, 10),
        ({"a": 4, "t": 2}, None, None, 0, 0),
        ({"b": {"$in": [1, 2, 3]}, "a": {"$gt": 10}}, [("b", -1)], None, 0, 0),
        ({"$or": [{"a": 1, "b": 2}, {"a": {"$lt": 3}, "b": {"$gt": 4}}]}, None, None, 0, 0),
    ]
    out = []
    for f, s, pr, sk, li in queries:
        for _ in range(2):
            cur = col.find(f, pr, skip=sk, limit=li)
            if s:
                cur = cur.sort(s)
            out.append([d.get("_id") for d in cur])
    out.append(stats(col))
    return out


@pytest.mark.backlog_fixed("RM-154")
def test_plan_cache_stats_lists_the_entries(rust_mongo: Any, mongod: Any) -> None:
    """Each entry as mongod 8.2.11 lists it: hashes, `isActive`, `works`,
    `createdFromQuery`, `cachedPlan`, each candidate's `creationExecStats`
    (the trial's own per-stage counts), `candidatePlanScores` (the tie
    breaking bonuses included), `indexFilterSet`, the latest used first."""
    assert _entries(rust_mongo) == _entries(mongod)


# ------------------------------------- what clears it, and what does not --


def _cleared(client: Any) -> list:
    col = fresh(
        client, "rm154clear", n=1500, gen=lambda i: {"_id": i, "a": i % 100, "b": (i // 20) % 100}
    )
    db = col.database
    q = {"a": {"$lt": 5}, "b": {"$lt": 90}}

    def twice(query: Any = q, **kw: Any) -> None:
        for _ in range(2):
            list(col.find(query, **kw))

    out = []
    col.create_index("c", expireAfterSeconds=100)
    twice()
    out.append(summary(col))
    steps = [
        lambda: db.command(
            "collMod", "c", index={"keyPattern": {"c": 1}, "expireAfterSeconds": 200}
        ),
        lambda: db.command("collMod", "c", validationLevel="moderate"),
        lambda: col.create_index("d"),
        lambda: db.command("collMod", "c", index={"name": "d_1", "hidden": True}),
        lambda: col.insert_one({"_id": -1, "a": [1, 2], "b": 5}),
        lambda: col.drop_index("d_1"),
        lambda: db.command("planCacheClear", "c", query={"a": {"$lt": 1}, "b": {"$lt": 2}}),
        lambda: db.command("planCacheClear", "c", query=q, sort={"a": 1}),
        lambda: db.command("planCacheClear", "c"),
        lambda: db.command("planCacheClear", "c", query=5),
        lambda: db.command("planCacheClear", "c", sort={"a": 1}),
        lambda: db.command("planCacheClear", "c", query=q, collation={}),
        lambda: db.command("planCacheClear", "nope"),
        lambda: db.command("planCacheSetFilter", "c", query=q, indexes=[{"a": 1}, {"b": 1}]),
        lambda: db.command("planCacheClearFilters", "c"),
    ]
    for step in steps:
        out.append(ok(step))
        out.append(summary(col))
        twice()
        out.append(summary(col))
    twice(collation={"locale": "fr"})
    twice(projection={"a": 1})
    out.append(summary(col))
    out.append(ok(lambda: db.command("planCacheClear", "c", query=q, collation={"locale": "fr"})))
    out.append(summary(col))
    out.append(ok(lambda: db.command("planCacheClear", "c", query=q, projection={"a": 1})))
    out.append(summary(col))
    col.drop()
    out.append(summary(col))
    return [x if not isinstance(x, tuple) else x[:2] for x in out]


@pytest.mark.backlog_fixed("RM-154")
def test_what_clears_the_plan_cache(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 clears a collection's cache when an index is built,
    dropped, hidden, has its TTL changed, or becomes multikey -- not on a
    collMod of the validator -- and `planCacheClear` clears all of it or one
    shape (query, sort, projection, collation); setting or clearing an index
    filter clears that shape. A dropped collection has no cache (50933)."""
    assert _cleared(rust_mongo) == _cleared(mongod)


def _evicted(client: Any) -> Any:
    col = fresh(client, "rm154lru", n=60, gen=lambda i: {"_id": i, "a": i % 10, "b": i % 7})
    for n in range(5002):
        list(col.find({"a": {"$lt": 5}, "b": {"$lt": 3}}).sort([(f"f{n}", 1)]))
        if n == 10:
            list(col.find({"a": {"$lt": 5}, "b": {"$lt": 3}}).sort([("f0", 1)]))
    listed = answer(lambda: list(col.aggregate([{"$planCacheStats": {}}])))
    if isinstance(listed, tuple):
        return listed[:2]
    sorts = [list(e["createdFromQuery"]["sort"])[0] for e in listed]
    return len(sorts), sorts[:3], sorts[-3:], "f0" in sorts, "f1" in sorts, "f2" in sorts


@pytest.mark.backlog_fixed("RM-154")
def test_the_cache_keeps_5000_entries(rust_mongo: Any, mongod: Any) -> None:
    """`internalQueryCacheMaxEntriesPerCollection`: 5000, the least
    recently used evicted (mongod 8.2.11: of 5002 shapes, the first two
    go, unless used again since)."""
    assert _evicted(rust_mongo) == _evicted(mongod)


# --------------------------------------------- RM-215: the stage itself --


def _stage(client: Any) -> list:
    col = fresh(client, "rm154stage", n=20, gen=lambda i: {"_id": i, "a": i % 4, "b": i % 3})
    db = col.database
    for _ in range(2):
        list(col.find({"a": {"$lt": 2}, "b": 1}))
    out = [
        answer(lambda: len(list(col.aggregate([{"$planCacheStats": {}}])))),
        answer(lambda: list(db.nope.aggregate([{"$planCacheStats": {}}]))),
        answer(lambda: list(col.aggregate([{"$planCacheStats": {"x": 1}}]))),
        answer(lambda: list(col.aggregate([{"$planCacheStats": {"allHosts": True}}]))),
        answer(lambda: list(col.aggregate([{"$planCacheStats": 1}]))),
        answer(lambda: list(col.aggregate([{"$match": {}}, {"$planCacheStats": {}}]))),
        answer(
            lambda: [
                e["isActive"]
                for e in col.aggregate([{"$planCacheStats": {}}, {"$match": {"isActive": True}}])
            ]
        ),
        answer(
            lambda: list(
                col.aggregate([{"$planCacheStats": {}}, {"$match": {"host": {"$exists": True}}}])
            )
        ),
        answer(
            lambda: [
                sorted(k for k in e if k != "estimatedSizeBytes")
                for e in col.aggregate([{"$planCacheStats": {}}, {"$project": {"_id": 0}}])
            ]
        ),
    ]
    return [x[:2] if isinstance(x, tuple) else x for x in out]


@pytest.mark.backlog_fixed("RM-215")
def test_plan_cache_stats_stage(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs `$planCacheStats` as the first stage only (40602),
    on a collection that exists (50933), with at most `allHosts`, and that
    only on a sharded cluster (9, 4503200); a `$match` right after it is
    absorbed, so it sees the entry before `host` is added."""
    assert _stage(rust_mongo) == _stage(mongod)
