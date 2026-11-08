"""The plan cache against mongod 8.2.11 live: what it did not reproduce
(RM-341) and what it now does (RM-340, RM-342)."""

from __future__ import annotations

from typing import Any

import pytest
from pymongo.errors import PyMongoError


def _listed(col: Any) -> Any:
    try:
        return list(col.aggregate([{"$planCacheStats": {}}]))
    except PyMongoError as e:
        return ("err", getattr(e, "code", None))


def _fresh(client: Any, db: str, n: int) -> Any:
    client.drop_database(db)
    col = client[db].c
    col.insert_many([{"_id": i, "a": i % 100, "b": (i // 20) % 100} for i in range(n)])
    col.create_index("a")
    col.create_index("b")
    return col


def _summary(col: Any) -> Any:
    got = _listed(col)
    if isinstance(got, tuple):
        return got
    return [
        (
            e["planCacheShapeHash"],
            e["planCacheKey"],
            e["isActive"],
            e["works"],
            e["worksType"],
            len(e["candidatePlanScores"]),
        )
        for e in got
    ]


def _sbe(client: Any) -> Any:
    col = _fresh(client, "rm340", 2000)
    q = {"a": {"$lt": 5}, "b": {"$lt": 90}}
    for _ in range(2):
        list(col.aggregate([{"$match": q}, {"$group": {"_id": "$b", "n": {"$sum": 1}}}]))
    first = _summary(col)
    col.database.command("planCacheClear", "c")
    for _ in range(2):
        col.count_documents(q)
    return first, _summary(col)


@pytest.mark.backlog_fixed("RM-340")
def test_a_query_sbe_runs_is_cached_with_its_reads(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs a `$match` and the `$group` pushed behind it in
    SBE, planned by the classic planner without index intersections, and
    caches the winner under a key ending `s` with `worksType: "reads"` --
    the keys and documents its trial read (`count_documents` is such a
    pipeline). The front was not cached at all here."""
    assert _sbe(rust_mongo) == _sbe(mongod)


def _group(col: Any, q: Any, limit: int = 0) -> Any:
    stages: list = [{"$match": q}]
    if limit:
        stages.append({"$limit": limit})
    stages.append({"$group": {"_id": "$b", "n": {"$sum": 1}}})
    return list(col.aggregate(stages))


def _sbe_steps(client: Any, db: str, steps: list) -> Any:
    col = _fresh(client, db, 2000)
    seen = []
    for q, limit in steps:
        _group(col, q, limit)
        seen.append(
            [
                (s, e["planCacheKey"], e["cachedPlan"]["inputStage"]["inputStage"]["indexName"])
                for s, e in zip(_summary(col), _listed(col), strict=True)
            ]
        )
    return seen


_RANGE = {"a": {"$lt": 5}, "b": {"$lt": 90}}


@pytest.mark.backlog_fixed("RM-340")
def test_a_cached_sbe_plan_is_replanned_at_ten_times_its_reads(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 keeps an active SBE entry's plan while it finishes
    within ten times the reads it was cached with, and replans when its
    reads reach that: cached at 200 reads on `a_1`, a query `a_1` answers
    in 1,960 reads keeps it, one it answers in 2,000 replans to `b_1`; under
    a `$limit` the entry's reads are the trial's to that limit."""
    steps = [
        (_RANGE, 0),
        (_RANGE, 0),
        ({"a": {"$lt": 49}, "b": {"$lt": 5}}, 0),
        ({"a": {"$lt": 50}, "b": {"$lt": 5}}, 0),
        ({"a": {"$lt": 5}, "b": {"$lt": 90}}, 7),
        ({"a": {"$lt": 50}, "b": {"$lt": 90}}, 0),
    ]
    assert _sbe_steps(rust_mongo, "rm340r", steps) == _sbe_steps(mongod, "rm340r", steps)


def _no_intersection(client: Any) -> Any:
    col = _fresh(client, "rm340i", 2000)
    col.update_many({}, [{"$set": {"t": [{"$mod": ["$_id", 3]}, {"$mod": ["$_id", 4]}]}}])
    col.create_index("t")
    seen = []
    for q in ({"a": 3, "b": 5}, {"t": {"$all": [1, 2]}, "a": {"$lt": 50}}):
        for _ in range(2):
            _group(col, q)
        pipeline = [{"$match": q}, {"$group": {"_id": "$b"}}]
        explained = col.database.command(
            "explain",
            {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            verbosity="queryPlanner",
        )
        planner = explained.get("queryPlanner") or explained["stages"][0]["$cursor"]["queryPlanner"]
        seen.append((_summary(col)[0], len(planner["rejectedPlans"])))
    return seen


@pytest.mark.backlog_fixed("RM-340")
def test_a_query_sbe_runs_tries_no_index_intersection(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 plans a front it runs in SBE without index
    intersections: `{a: 3, b: 5}` over `a_1` and `b_1` tries the two scans
    and no `AND_SORTED`, and `{t: {$all: [1, 2]}}` over a multikey `t_1`
    tries its two scans and no intersection of them -- in the cache entry
    and in `explain`'s rejected plans alike."""
    assert _no_intersection(rust_mongo) == _no_intersection(mongod)


def _sizes(client: Any) -> Any:
    col = _fresh(client, "rm341", 2000)
    for _ in range(2):
        list(col.find({"a": {"$lt": 5}, "b": {"$lt": 90}}))
    got = _listed(col)
    if isinstance(got, tuple):
        return got
    return [e.get("estimatedSizeBytes") for e in got]


@pytest.mark.backlog("RM-341")
def test_an_entry_reports_its_estimated_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 lists each entry's `estimatedSizeBytes`: the bytes its
    plan cache entry takes in mongod's memory (3040 for this one)."""
    assert _sizes(rust_mongo) == _sizes(mongod)


def _large(client: Any) -> Any:
    col = _fresh(client, "rm342", 2500)
    for _ in range(2):
        list(col.find({"a": {"$lt": 5}, "b": {"$lt": 90}}))
    return _summary(col)


@pytest.mark.backlog_fixed("RM-342")
def test_a_collection_over_2000_documents_is_cached(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 tries the candidate plans, and caches the winner,
    whatever the collection's size; the trial here ran over at most 2000
    documents, and past that nothing was cached. It now reads, past that
    size, what the candidates' scans reach in the store."""
    assert _large(rust_mongo) == _large(mongod)
