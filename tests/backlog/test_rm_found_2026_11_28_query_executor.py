"""A gap found while making RM-610's regression test count rather than time
(RM-728, 2026-11-28): mongod 8.2.11 counts the work each read does in
`serverStatus().metrics.queryExecutor`, and this server keeps no such
section.

The reference is mongod, asked live (`mongod`, a standalone of its own); the
Rust server is `secantusd-mdb` (`rust_mongo`). Each server's counters are
read before and after the same `find`, so earlier tests' work on the same
server does not show.
"""

from __future__ import annotations

from typing import Any

import pytest


def _query_executor(client: Any) -> dict:
    return client.admin.command("serverStatus").get("metrics", {}).get("queryExecutor", {})


def _moved(before: dict, after: dict) -> Any:
    """`after - before`, leaf by leaf, for the leaves mongod keeps."""
    if not isinstance(after, dict):
        return after - before if isinstance(before, int) else None
    return {k: _moved(before.get(k, {} if isinstance(v, dict) else 0), v) for k, v in after.items()}


def _collscan_then_contested(client: Any) -> list:
    db = client["rm_found_query_executor"]
    db.drop_collection("c")
    db.c.insert_many([{"_id": i, "a": i, "b": i % 7} for i in range(10_000)])
    out = []
    try:
        before = _query_executor(client)
        list(db.c.find({"a": {"$gt": 10}}).limit(50))
        out.append(_moved(before, _query_executor(client)))
        db.c.create_index("a")
        db.c.create_index("b")
        before = _query_executor(client)
        list(db.c.find({"a": {"$gte": 0}, "b": {"$gte": 0}}).limit(101))
        out.append(_moved(before, _query_executor(client)))
    finally:
        client.drop_database("rm_found_query_executor")
    return out


@pytest.mark.backlog("RM-846")
def test_a_find_moves_the_query_executor_counters_as_mongod(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 (measured 2026-11-28): a `find` with a limit of 50 of
    `{a: {$gt: 10}}` with no index moves `metrics.queryExecutor` by 0 keys
    (`scanned`), 61 documents (`scannedObjects`) and one collection scan; a
    contested `find` with a limit of 101 between indexes on `a` and `b` by
    101 keys and 101 documents. This server's `metrics` has no
    `queryExecutor`."""
    theirs = _collscan_then_contested(mongod)
    assert theirs[1]["scanned"] == 101, theirs
    assert _collscan_then_contested(rust_mongo) == theirs
