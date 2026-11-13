"""Gap test for what working RM-402 found, 2026-11-10: `find`, a leading
`$sort` and `$setWindowFields` hand back documents whose sort keys tie in
the order they were read, where mongod's sorter reorders them.
"""

from __future__ import annotations

from typing import Any

import pytest


def _tied_order(client: Any) -> dict:
    client.drop_database("rmtie")
    c = client.rmtie.c
    c.insert_many([{"_id": i, "k": i % 3} for i in range(40)])
    window = {"sortBy": {"k": 1}, "output": {"r": {"$documentNumber": {}}}}
    return {
        "find": [d["_id"] for d in c.find({}).sort("k", 1)],
        "leading $sort": [d["_id"] for d in c.aggregate([{"$sort": {"k": 1}}])],
        "$setWindowFields": [d["_id"] for d in c.aggregate([{"$setWindowFields": window}])],
    }


@pytest.mark.backlog("RM-527")
def test_tied_order_of_find_leading_sort_and_window(rust_mongo: Any, mongod: Any) -> None:
    """mongod sorts all three with its sorter's `std::sort`, so the 40
    documents over three values of `k` come back reordered -- macOS mongod
    8.2.11: 39, 3, 27, 9, ...; libstdc++'s order on Linux: 0, 21, 18, 24,
    ... (g++ 15.2) -- where this server's `$setWindowFields` returns them in
    the order read. `find` and a leading `$sort` agree since RM-312 (Linux)
    and RM-526 (macOS)."""
    assert _tied_order(rust_mongo) == _tied_order(mongod)
