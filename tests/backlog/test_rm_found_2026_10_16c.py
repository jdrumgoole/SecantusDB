"""Gap tests for Rust MongoDB server items found on 2026-10-16 during the
third backlog cycle (measured against mongod 8.2.11)."""

from __future__ import annotations

import random
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _inputs() -> list[list[float]]:
    """Multiples of 1/8: exact in every mongod build (the note above
    test_std_dev_over_range_windows_and_non_finite_values says why; RM-231)."""
    rng = random.Random(3)
    return [[rng.randint(-8000, 8000) / 8 for _ in range(rng.randint(3, 10))] for _ in range(20)]


def _fresh_sample_std_devs(xs: list[float]) -> list:
    """Each `documents: [-2, 0]` window's sample deviation, computed afresh
    and rounded once (None for a window of one)."""
    import statistics

    out = []
    for j in range(len(xs)):
        window = xs[max(0, j - 2) : j + 1]
        out.append(statistics.stdev(window) if len(window) > 1 else None)
    return out


def _sliding_std_dev(client: Any) -> list:
    out = []
    for xs in _inputs():
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
    theirs = _sliding_std_dev(mongod)
    # Not vacuous: somewhere mongod's incremental answer differs from a
    # correctly rounded fresh one, which is what this test pins.
    fresh = [("ok", _fresh_sample_std_devs(xs)) for xs in _inputs()]
    assert theirs != fresh, "no window's last digit shows the incremental arithmetic"
    assert _sliding_std_dev(rust_mongo) == theirs
