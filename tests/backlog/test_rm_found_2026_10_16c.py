"""Gap tests for Rust MongoDB server items found on 2026-10-16 during the
third backlog cycle (measured against mongod 8.2.11)."""

from __future__ import annotations

import random
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _sliding_std_dev(client: Any) -> list:
    rng = random.Random(3)
    out = []
    for _ in range(20):
        xs = [rng.uniform(-1e3, 1e3) for _ in range(rng.randint(3, 10))]
        pipeline = [
            {"$documents": [{"i": j, "v": x} for j, x in enumerate(xs)]},
            {
                "$setWindowFields": {
                    "sortBy": {"i": 1},
                    "output": {"s": {"$stdDevSamp": "$v", "window": {"documents": [-2, 0]}}},
                }
            },
        ]
        out.append(mongo_answer(lambda p=pipeline: [d["s"] for d in client.admin.aggregate(p)]))
    return out


@pytest.mark.backlog_fixed("RM-181")
def test_std_dev_over_a_sliding_window(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 computes `$stdDevSamp` over a window that drops
    documents (`documents: [-2, 0]`) incrementally, and its last digit
    differs from a fresh computation over each window. A window that only
    grows matches either way."""
    assert _sliding_std_dev(rust_mongo) == _sliding_std_dev(mongod)
