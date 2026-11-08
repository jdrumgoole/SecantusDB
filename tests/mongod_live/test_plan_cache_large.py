"""mongod's plan cache over a collection larger than the 2,000 documents
this server once read whole for a trial (RM-342).

mongod runs its candidate plans against each other whatever the
collection's size, each for at most `max(10000, 0.3 * documents)` works,
and caches the winner. Here a trial over a large collection reads only
what the candidates' scans reach in the store, so these cases pin that it
counts what mongod counts: each candidate's per-stage work, its score, the
works the entry is cached with, and its activation on the second run --
across a selective point query, ranges that end the trial at 101
documents, a scan of an `$in` that seeks, a multikey index, a sort an
index walk answers, a limit, and a query no plan answers, whose trial runs
its whole 12,000 works over 40,000 documents (mongod 8.2.11, 2026-11-08).
A compound scan whose second field has several intervals is RM-433's.
"""

from __future__ import annotations

from typing import Any

from tests.mongod_live.conftest import same

DB = "live_plan_cache_large"
N = 40_000

#: Per run (`solutionHash` is seeded per process), or what this server
#: does not keep (RM-341).
VOLATILE = {
    "timeOfCreation",
    "solutionHash",
    "estimatedSizeBytes",
    "executionTimeMillisEstimate",
    "saveState",
    "restoreState",
    "host",
}


def strip(x: Any) -> Any:
    if isinstance(x, dict):
        return {k: strip(v) for k, v in x.items() if k not in VOLATILE}
    if isinstance(x, list):
        return [strip(v) for v in x]
    return x


def _setup(client: Any) -> Any:
    client.drop_database(DB)
    col = client[DB].c
    col.insert_many(
        [
            {"_id": i, "a": i % 100, "b": (i // 20) % 100, "c": i % 7, "t": [i % 3, i % 5]}
            for i in range(N)
        ]
    )
    for ix in ("a", "b", [("a", 1), ("c", 1)], "t"):
        col.create_index(ix)
    return col


CASES = [
    {"filter": {"a": 4, "b": 7}},
    {"filter": {"a": {"$lt": 5}, "b": {"$lt": 90}}},
    {"filter": {"a": {"$in": [3, 60]}, "b": {"$gte": 40}}},
    {"filter": {"a": 4, "t": 2}},
    {"filter": {"a": {"$gte": 3}, "b": 2}, "sort": {"a": 1}},
    {"filter": {"a": {"$lt": 50}, "b": {"$lt": 50}}, "limit": 7},
    {"filter": {"a": {"$gte": 0}, "b": {"$gte": 0}, "z": 1}},
]


def _cached(client: Any) -> list:
    col = _setup(client)
    out = []
    for case in CASES:
        col.database.command("planCacheClear", "c")
        seen = []
        for _ in range(2):
            cursor = col.find(case["filter"])
            if "sort" in case:
                cursor = cursor.sort(list(case["sort"].items()))
            if "limit" in case:
                cursor = cursor.limit(case["limit"])
            list(cursor)
            seen.append(strip(list(col.aggregate([{"$planCacheStats": {}}]))))
        out.append(seen)
    return out


def test_a_large_collections_trial_is_cached_as_mongod_caches_it(
    rust_mongo: Any, mongod: Any
) -> None:
    same(_cached(rust_mongo), _cached(mongod))
