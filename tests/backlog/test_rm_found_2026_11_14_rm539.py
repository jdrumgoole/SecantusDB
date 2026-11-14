"""Gap tests for what working RM-539 (what explain's stages inside a
`$facet`, a `$unionWith` and a `$lookup` report) found on 2026-11-14,
comparing with mongod 8.2.11 (a one-member replica set, macOS arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture


def _without_times(value: Any) -> Any:
    """The answer less what the machine's clock decides."""
    if isinstance(value, dict):
        return {
            k: _without_times(v)
            for k, v in value.items()
            if k not in ("executionTimeMillisEstimate", "executionTimeMillis")
        }
    if isinstance(value, list):
        return [_without_times(v) for v in value]
    return value


def _collections(client: Any) -> Any:
    client.drop_database("rm539f")
    db = client.rm539f
    db.c.insert_many([{"_id": n, "a": n} for n in range(1, 4)])
    db.d.insert_many([{"_id": n, "b": n} for n in range(1, 3)])
    return db


def _explained(db: Any, target: Any, pipeline: list, verbosity: str, stage: str) -> Any:
    def run() -> Any:
        reply = db.command(
            "explain",
            {"aggregate": target, "pipeline": pipeline, "cursor": {}},
            verbosity=verbosity,
        )
        return _without_times([s for s in reply.get("stages", []) if stage in s])

    return mongo_answer(run)


# ------------------------------------- a $unionWith over a collection --


def _union_over_a_collection(client: Any) -> dict:
    db = _collections(client)
    pipelines = {
        "named": [{"$unionWith": "d"}],
        "with_pipeline": [{"$unionWith": {"coll": "d", "pipeline": [{"$match": {"b": 1}}]}}],
    }
    return {
        f"{name}/{v}": _explained(db, "c", p, v, "$unionWith")
        for name, p in pipelines.items()
        for v in ("queryPlanner", "executionStats")
    }


@pytest.mark.backlog("RM-681")
def test_a_union_with_over_a_collection_lists_its_cursor(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 lists a `$unionWith` over a collection with its
    pipeline as it runs it: a `$cursor` holding the query's own
    `queryPlanner` (and, run, its `executionStats`), then the stages after
    it, each with its `nReturned`. This server lists the stage as
    written."""
    assert _union_over_a_collection(rust_mongo) == _union_over_a_collection(mongod_rs)


# ---------------------------------------- a $lookup over a collection --

EXAMINED = ("totalDocsExamined", "totalKeysExamined", "collectionScans", "indexesUsed")


def _lookup_examined(client: Any) -> dict:
    db = _collections(client)
    pipelines = {
        "uncorrelated": [{"$lookup": {"from": "d", "pipeline": [{"$match": {"b": 1}}], "as": "z"}}],
        "correlated": [
            {
                "$lookup": {
                    "from": "d",
                    "let": {"x": "$a"},
                    "pipeline": [{"$match": {"$expr": {"$eq": ["$b", "$$x"]}}}],
                    "as": "z",
                }
            }
        ],
    }
    out = {}
    for name, p in pipelines.items():
        answer = _explained(db, "c", p, "executionStats", "$lookup")
        out[name] = (
            [{k: s.get(k) for k in EXAMINED} for s in answer[1]] if answer[0] == "ok" else answer
        )
    return out


@pytest.mark.backlog("RM-682")
def test_a_lookup_over_a_collection_reports_what_it_examined(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 gives a `$lookup` whose pipeline reads a collection
    what the reads examined, summed over its runs: an uncorrelated
    pipeline's documents once (2 documents, 1 collection scan for 3 outer
    documents: it is run once and cached), a correlated one's at every
    run. This server reports none of the four figures."""
    assert _lookup_examined(rust_mongo) == _lookup_examined(mongod_rs)


# ------------------------------------------ a $lookup SBE runs --


def _join_field_lookup(client: Any) -> Any:
    db = _collections(client)

    def run() -> dict:
        reply = db.command(
            "explain",
            {
                "aggregate": "c",
                "pipeline": [
                    {"$lookup": {"from": "d", "localField": "a", "foreignField": "b", "as": "z"}}
                ],
                "cursor": {},
            },
            verbosity="queryPlanner",
        )
        plan = reply.get("queryPlanner", {}).get("winningPlan", {})
        return {
            "explainVersion": reply.get("explainVersion"),
            "stages": "stages" in reply,
            "stage": plan.get("queryPlan", {}).get("stage"),
        }

    return mongo_answer(run)


@pytest.mark.backlog("RM-683")
def test_a_lookup_on_join_fields_is_explained_as_sbe_runs_it(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 pushes a `$lookup` on `localField` / `foreignField`
    over a collection into SBE and explains it so: `explainVersion` "2", no
    `stages`, an `EQ_LOOKUP` on top of the query's plan. This server lists
    a classic `$cursor` and a `$lookup` stage."""
    assert _join_field_lookup(rust_mongo) == _join_field_lookup(mongod_rs)


# ------------------------------------------------- a short string --


def _twelve_byte_strings(client: Any) -> dict:
    client.drop_database("rm539s")
    db = client.rm539s
    db.c.insert_one({"a": 1, "b": "v" * 12})
    grouped = _explained(
        db,
        "c",
        [
            {"$_internalInhibitOptimization": {}},
            {"$group": {"_id": None, "x": {"$push": "$b"}}},
        ],
        "executionStats",
        "$group",
    )
    sorted_ = _explained(
        client.admin,
        1,
        [{"$documents": [{"a": 1, "b": "v" * 12}]}, {"$sort": {"a": 1}}],
        "executionStats",
        "$sort",
    )
    return {
        "group": [s.get("maxAccumulatorMemoryUsageBytes") for s in grouped[1]]
        if grouped[0] == "ok"
        else grouped,
        "sort": [s.get("totalDataSizeSortedBytesEstimate") for s in sorted_[1]]
        if sorted_[0] == "ok"
        else sorted_,
    }


@pytest.mark.backlog("RM-684")
def test_a_twelve_byte_string_is_kept_in_its_value(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11's classic `Value` keeps a string of up to 12 bytes
    inline: a `$push` of one counts 136 bytes, a `$sort` of a `$documents`
    document holding one 245. This server keeps only 11 inline
    (`memlimit::SHORT_STR`) and counts 36 bytes more for each."""
    assert _twelve_byte_strings(rust_mongo) == _twelve_byte_strings(mongod_rs)


# --------------------------------------- a $sort and $limit in a facet --


def _facet_sort_limit(client: Any) -> Any:
    return _explained(
        client.admin,
        1,
        [
            {"$documents": [{"a": 1}, {"a": 2}]},
            {"$facet": {"x": [{"$sort": {"a": 1}}, {"$limit": 1}]}},
        ],
        "executionStats",
        "$facet",
    )


@pytest.mark.backlog("RM-685")
def test_a_sort_and_its_limit_in_a_facet_are_one_stage(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 folds a `$limit` into the `$sort` before it inside a
    `$facet`'s pipeline too, and the top-1 sort reports no bytes sorted
    (`{$sort: {sortKey: {a: 1}, limit: 1}}`, `totalDataSizeSortedBytesEstimate`
    0). This server lists the two stages, the sort counting both documents
    (490). The fold outside a facet is RM-538."""
    assert _facet_sort_limit(rust_mongo) == _facet_sort_limit(mongod_rs)


# --------------------------------- what a $documents sort counts, run --


def _sorted_bytes(client: Any) -> int:
    def total() -> int:
        return client.admin.command("serverStatus")["metrics"]["query"]["sort"]["totalBytesSorted"]

    before = total()
    list(
        client.admin.aggregate([{"$documents": [{"a": 1, "b": 3}, {"a": 2}]}, {"$sort": {"a": 1}}])
    )
    return total() - before


@pytest.mark.backlog("RM-686")
def test_a_documents_sort_counts_the_documents_built(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 counts each document a `$documents` makes as the
    storage its expression object built: `metrics.query.sort.totalBytesSorted`
    grows by 490 for `[{a: 1, b: 3}, {a: 2}]` sorted. This server's explain
    counts them so (RM-539), but a run hands them to the pipeline as read
    BSON and counts 255."""
    assert _sorted_bytes(rust_mongo) == _sorted_bytes(mongod_rs)
