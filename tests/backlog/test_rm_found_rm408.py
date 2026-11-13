"""Gap tests for what modelling mongod's document storage (RM-408,
2026-11-10) found beside it, compared with a live mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

WINDOW = {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"z": {"$first": "$n"}}}}


def _load(client: Any) -> Any:
    client.drop_database("rmfoundrm408")
    db = client.rmfoundrm408
    db.c.insert_many(
        [
            {
                "_id": i,
                "a": 5,
                "b": "y" * 100,
                "e": {"x": 1, "y": "w" * 50},
                "t": [1, "z" * 30, {"q": 1}],
                "n": i,
            }
            for i in range(300)
        ]
    )
    return db


def _stats(db: Any, pipeline: list) -> list:
    ex = db.command(
        {
            "explain": {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            "verbosity": "executionStats",
        }
    )
    return ex["stages"][1:]


def _sub_document_sizes(client: Any) -> dict:
    db = _load(client)
    out = {}
    for name, first in {
        "a field read inside a sub-document": {"$addFields": {"q": "$e.y"}},
        "a field set inside a sub-document": {"$set": {"e.z": 1}},
    }.items():
        stages = _stats(db, [first, WINDOW])
        out[name] = (
            stages[1]["totalDataSizeSortedBytesEstimate"],
            stages[2]["maxTotalMemoryUsageBytes"],
        )
    return out


@pytest.mark.backlog("RM-515")
def test_a_sub_document_a_stage_touched_is_sized_by_its_storage(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 gives a sub-document read through (`$e.y`) or written
    into (`e.z`) a cache of its own, which the window's sort and cache
    count: (349500, 369000) and (305100, 324600) bytes over 300
    documents."""
    assert _sub_document_sizes(rust_mongo) == _sub_document_sizes(mongod)


def _sorted_with_limit(client: Any) -> int:
    db = _load(client)
    stages = _stats(db, [{"$addFields": {"q": "$b"}}, {"$sort": {"n": -1}}, {"$limit": 200}])
    return stages[1]["totalDataSizeSortedBytesEstimate"]


@pytest.mark.backlog("RM-516")
def test_explain_counts_the_documents_a_sort_with_a_limit_kept(
    rust_mongo: Any, mongod: Any
) -> None:
    """A pipeline `$sort` that takes the `$limit` behind it keeps only that
    many documents, and mongod 8.2.11's `explain` counts those: 200 of
    657 bytes each, 131400."""
    assert _sorted_with_limit(rust_mongo) == _sorted_with_limit(mongod)


def _listed(client: Any) -> list:
    db = _load(client)
    stages = _stats(db, [{"$unwind": "$t"}, {"$sort": {"t": 1}}, WINDOW])
    return [next(k for k in s if k.startswith("$")) for s in stages]


@pytest.mark.backlog_fixed("RM-517")
def test_a_sort_a_window_supersedes_is_not_listed(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 drops a `$sort` that the `$sort` a `$setWindowFields`
    is parsed into follows, and moves that one ahead of the `$unwind`:
    `explain` lists `$unwind` and `$_internalSetWindowFields` alone. This
    server listed and counted both `$sort`s, until RM-524 dropped the
    overridden one as mongod's optimizer does."""
    assert _listed(rust_mongo) == _listed(mongod)


def _inhibited(client: Any) -> Any:
    db = _load(client)
    pipeline = [{"$_internalInhibitOptimization": {}}, {"$sort": {"n": -1}}, {"$limit": 2}]
    return mongo_answer(lambda: [d["_id"] for d in db.c.aggregate(pipeline)])


@pytest.mark.backlog("RM-518")
def test_internal_inhibit_optimization_runs(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs `$_internalInhibitOptimization`, a stage that
    passes its documents on and keeps the stages after it out of the scan:
    the answer is that of the pipeline without it."""
    assert _inhibited(rust_mongo) == _inhibited(mongod)


def _projected_sort(client: Any) -> int:
    db = _load(client)
    stages = _stats(db, [{"$unwind": "$t"}, {"$project": {"t": 1, "n": 1}}, {"$sort": {"t": -1}}])
    return stages[-1]["totalDataSizeSortedBytesEstimate"]


@pytest.mark.backlog("RM-519")
def test_a_document_a_project_built_is_sized_as_built(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's inclusion `$project` behind an `$unwind` builds each
    document it returns, and the `$sort` after it counts that storage:
    351300 bytes over 900 documents."""
    assert _projected_sort(rust_mongo) == _projected_sort(mongod)
