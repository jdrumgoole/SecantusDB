"""Gap test for what working RM-527 found, 2026-11-14: a `$setWindowFields`
whose `$sort` mongod hoists ahead of an `$unwind` sorts the documents the
`$unwind` took, so the documents one of them unwinds into stay together, in
`std::sort`'s order of their source documents.
"""

from __future__ import annotations

from typing import Any

import pytest


def _unwound_window_order(client: Any) -> list:
    client.drop_database("rmtieunwind")
    c = client.rmtieunwind.c
    c.insert_many([{"_id": i, "k": i % 3, "a": [i % 4, 9]} for i in range(60)])
    pipeline = [
        {"$unwind": "$a"},
        {"$setWindowFields": {"sortBy": {"k": 1}, "output": {"r": {"$documentNumber": {}}}}},
    ]
    return [(d["_id"], d["a"]) for d in c.aggregate(pipeline)]


@pytest.mark.backlog("RM-640")
def test_window_sort_hoisted_past_unwind_orders_the_source_documents(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 (macOS) runs the window's `$sort` on `k` before the
    `$unwind` of `a`, so ties come in `std::sort`'s order of the 60 source
    documents, each followed by its second element: (0, 0), (0, 9), (3, 3),
    (3, 9), (30, 2), (30, 9), .... This server sorts nothing there and
    returns them in the order read: (0, 0), (0, 9), (3, 3), (3, 9), (6, 2),
    ...."""
    assert _unwound_window_order(rust_mongo) == _unwound_window_order(mongod)
