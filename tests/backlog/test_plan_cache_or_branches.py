"""The plan cache entries of a rooted `$or`'s branches, against mongod
8.2.11 live (found 2026-11-08 while working RM-340).

The subplanner plans and caches each branch of a rooted `$or` as a query of
its own. mongod keys the branch, and records it in `createdFromQuery`, with
the query's projection."""

from __future__ import annotations

from typing import Any

import pytest
from pymongo.errors import PyMongoError

_OR = {"$or": [{"a": 1, "b": 2}, {"a": {"$lt": 3}, "b": {"$lt": 50}}]}


def _fresh(client: Any, db: str) -> Any:
    client.drop_database(db)
    col = client[db].c
    col.insert_many([{"_id": i, "a": i % 100, "b": (i // 20) % 100} for i in range(2000)])
    col.create_index("a")
    col.create_index("b")
    return col


def _entries(col: Any) -> Any:
    try:
        listed = list(col.aggregate([{"$planCacheStats": {}}]))
    except PyMongoError as e:
        return ("err", getattr(e, "code", None))
    return [
        (
            e["planCacheShapeHash"],
            e["planCacheKey"],
            e["isActive"],
            e["works"],
            e["worksType"],
            e["createdFromQuery"],
        )
        for e in listed
    ]


def _projected(client: Any) -> Any:
    col = _fresh(client, "or_branch_projection")
    seen = []
    for read in (
        lambda: list(col.find(_OR, {"b": 1, "_id": 0})),
        lambda: list(col.find(_OR)),
        lambda: list(col.aggregate([{"$match": _OR}, {"$project": {"b": 1, "_id": 0}}])),
    ):
        col.database.command("planCacheClear", "c")
        for _ in range(2):
            read()
        seen.append(_entries(col))
    return seen


@pytest.mark.backlog_fixed("RM-430")
def test_an_or_branch_is_keyed_with_the_projection(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 caches a rooted `$or`'s branch under a key that holds
    the query's projection, and lists that projection in the entry's
    `createdFromQuery`, for a `find` and an aggregate's front alike. This
    server keyed and listed the branch with no projection."""
    assert _projected(rust_mongo) == _projected(mongod)


def _behind_sbe(client: Any) -> Any:
    col = _fresh(client, "or_branch_sbe")
    for _ in range(2):
        list(col.aggregate([{"$match": _OR}, {"$group": {"_id": "$b", "n": {"$sum": 1}}}]))
    return _entries(col)


@pytest.mark.backlog_fixed("RM-429")
def test_an_or_branch_before_sbe_stages_is_cached(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 caches the branch of a rooted `$or` in front of a
    `$group` it runs in SBE as a classic entry (`worksType: "works"`) under
    a key (`CA4248A6` here) whose shape ends in `o`, the flag
    `encodeClassic` gives a subplanner's branch of an SBE-compatible query,
    rather than `c` or `s`. This server cached no branch for such a
    pipeline."""
    assert _behind_sbe(rust_mongo) == _behind_sbe(mongod)
