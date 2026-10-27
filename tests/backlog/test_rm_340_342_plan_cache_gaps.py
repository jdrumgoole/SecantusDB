"""What the plan cache does not yet reproduce (RM-340, RM-341, RM-342),
against mongod 8.2.11 live."""

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


@pytest.mark.backlog("RM-340")
def test_a_query_sbe_runs_is_cached_with_its_reads(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs a `$match` and the `$group` pushed behind it in
    SBE, planned by the classic planner without index intersections, and
    caches the winner under a key ending `s` with `worksType: "reads"` --
    the keys and documents its trial read (`count_documents` is such a
    pipeline)."""
    assert _sbe(rust_mongo) == _sbe(mongod)


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


@pytest.mark.backlog("RM-342")
def test_a_collection_over_2000_documents_is_cached(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 tries the candidate plans, and caches the winner,
    whatever the collection's size; here the trial runs over at most 2000
    documents, and past that nothing is cached."""
    assert _large(rust_mongo) == _large(mongod)
