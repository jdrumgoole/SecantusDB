"""Gap tests for what modelling an inclusion `$project`'s documents
(RM-519, 2026-11-13) found beside it, compared with a live mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest


def _stages(client: Any, pipeline: list) -> list:
    client.drop_database("rmfoundrm519")
    db = client.rmfoundrm519
    db.c.insert_many(
        [
            {
                "_id": i,
                "a": 5,
                "b": "y" * 100,
                "e": {"x": 1, "y": "w" * 50, "z": {"k": 1}},
                "t": [1],
                "n": i,
            }
            for i in range(10)
        ]
    )
    ex = db.command(
        {
            "explain": {"aggregate": "c", "pipeline": pipeline, "cursor": {}},
            "verbosity": "executionStats",
        }
    )
    return [(next(iter(s)), s.get("totalDataSizeSortedBytesEstimate")) for s in ex["stages"][1:]]


def _excluded(client: Any) -> dict:
    return {
        str(spec): _stages(client, [{"$unwind": "$t"}, {"$project": spec}, {"$sort": {"n": 1}}])
        for spec in ({"b": 0}, {"e.y": 0})
    }


@pytest.mark.backlog("RM-637")
def test_a_sort_counts_what_an_exclusion_project_left(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's exclusion `$project` writes each excluded field out
    of the input's own storage, and projects a sub-document into one of its
    own: behind an `$unwind`, the `$sort` counts 4640 bytes over ten
    documents for `{b: 0}` and 6930 for `{"e.y": 0}`."""
    assert _excluded(rust_mongo) == _excluded(mongod)


def _after_pushed_group(client: Any) -> list:
    return _stages(
        client,
        [
            {"$group": {"_id": "$n", "c": {"$sum": 1}}},
            {"$project": {"c": 1}},
            {"$sort": {"c": 1}},
        ],
    )


@pytest.mark.backlog("RM-638")
def test_a_project_after_a_pushed_down_group_builds_its_documents(
    rust_mongo: Any, mongod: Any
) -> None:
    """Behind a `$group` mongod 8.2.11 runs in SBE, its inclusion
    `$project` builds each document it returns, and the `$sort` after it
    counts 2450 bytes over ten groups."""
    assert _after_pushed_group(rust_mongo) == _after_pushed_group(mongod)


def _after_window(client: Any) -> list:
    return [
        name
        for name, _ in _stages(
            client,
            [
                {"$unwind": "$t"},
                {"$set": {"w": 1}},
                {"$setWindowFields": {"sortBy": {"n": 1}, "output": {"z": {"$first": "$n"}}}},
                {"$sort": {"n": 1}},
            ],
        )
    ]


@pytest.mark.backlog("RM-639")
def test_a_sort_after_a_window_on_its_key_is_dropped(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 drops a `$sort` behind a `$setWindowFields` that
    sorted by the same key: behind an `$unwind` and a `$set`, its explain
    ends at the `$_internalSetWindowFields`."""
    assert _after_window(rust_mongo) == _after_window(mongod)
