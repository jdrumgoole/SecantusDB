"""A divergence found while converting RM-391's test to a count (RM-814,
2026-11-28): a single-document `update` or `delete` that matches nothing
tests its filter again.

A filter that draws `$rand` answers differently each time it runs, so the
share of statements that match counts how often the filter ran. mongod
8.2.11 runs it once per document: half of 1,500 statements over a
coin-flip filter match (720 and 715 measured, for an update and a delete).
This server matched about three quarters (1,133 and 1,125): a statement the
first test turns away is tried a second time.
"""

from __future__ import annotations

from typing import Any

import pytest

_N = 1000
_COIN = {"$expr": {"$lt": [{"$rand": {}}, 0.5]}}
#: Half of `_N` is what a filter run once matches; its standard deviation
#: is 16, so this is eight of them either side. A retried miss gives three
#: quarters.
_LOW, _HIGH = _N * 3 // 8, _N * 5 // 8


def _matched(client: Any) -> dict[str, int]:
    db = client["rm_single_write_retest"]
    out: dict[str, int] = {}
    try:
        db.c.insert_many([{"_id": i, "k": i, "n": 0} for i in range(_N)])
        statements = [{"$and": [{"k": i}, _COIN]} for i in range(_N)]
        out["update"] = db.command(
            "update", "c", updates=[{"q": q, "u": {"$inc": {"n": 1}}} for q in statements]
        )["n"]
        db.drop_collection("c")
        db.c.insert_many([{"_id": i, "k": i, "n": 0} for i in range(_N)])
        out["delete"] = db.command(
            "delete", "c", deletes=[{"q": q, "limit": 1} for q in statements]
        )["n"]
    finally:
        client.drop_database("rm_single_write_retest")
    return out


@pytest.mark.backlog("RM-855")
def test_a_single_update_or_delete_tests_a_miss_once(rust_mongo: Any, mongod_rs: Any) -> None:
    theirs = _matched(mongod_rs)
    assert all(_LOW <= v <= _HIGH for v in theirs.values()), theirs
    ours = _matched(rust_mongo)
    assert all(_LOW <= v <= _HIGH for v in ours.values()), (ours, theirs)
