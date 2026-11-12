"""Divergences found while working RM-374 (2026-11-09), each measured
against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest


def _keys_sorted(client: Any, pipeline: list) -> int:
    db = client["rm_found_rm374"]
    db.drop_collection("c")
    db.c.insert_many([{"_id": n, "x": n, "y": n % 3, "z": n % 2} for n in range(12)])

    def keys() -> int:
        return client.admin.command("serverStatus")["metrics"]["query"]["sort"]["totalKeysSorted"]

    before = keys()
    list(db.c.aggregate(pipeline))
    after = keys()
    client.drop_database("rm_found_rm374")
    return after - before


@pytest.mark.backlog_fixed("RM-468")
def test_an_aggregate_sort_with_a_limit_counts_every_key_it_sorted(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's top-k sort of an aggregate's leading `$sort` and
    `$limit` adds every document it read to
    `metrics.query.sort.totalKeysSorted` (6 for the six `{z: 0}` documents,
    12 with no `$match`), as a `find` with the same sort and limit does.
    This server's pipeline `$sort` counted only the k documents its read
    handed it for a limit of one, which never kept the read's input."""
    pipelines = {
        "match-limit-1": [{"$match": {"z": 0}}, {"$sort": {"y": 1}}, {"$limit": 1}],
        "match-limit-2": [{"$match": {"z": 0}}, {"$sort": {"y": 1}}, {"$limit": 2}],
        "limit-2": [{"$sort": {"y": 1}}, {"$limit": 2}],
    }
    answers = {
        "mongod": {k: _keys_sorted(mongod, p) for k, p in pipelines.items()},
        "rust": {k: _keys_sorted(rust_mongo, p) for k, p in pipelines.items()},
    }
    assert answers["mongod"] == {"match-limit-1": 6, "match-limit-2": 6, "limit-2": 12}
    assert answers["rust"] == answers["mongod"]
