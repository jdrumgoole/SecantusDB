"""Performance regression gates for the hot paths.

Each test runs a workload N times via ``pytest-benchmark`` (setup excluded
from timing) and asserts the median is under a hard upper-bound. The
limits are calibrated from observed in-process medians on a 2024-era
arm64 macOS dev machine with 50-100% headroom for run-to-run noise.

Excluded from the default ``pytest`` run via the ``perf`` marker —
benchmarks fight for CPU under ``pytest-xdist`` and would amplify
noise. Run via ``invoke perf`` (serial, no xdist) when you want to
gate against regressions, or directly via:

    uv run python -m pytest -p no:xdist -m perf tests/test_perf_regression.py

When you intentionally improve a hot path, lower the matching ``LIMIT_*``
in this file so the gate keeps tracking the floor.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer

pytestmark = pytest.mark.perf


# Hard upper bounds on the workload median, in milliseconds. Calibrated
# against post-perf-pass numbers (commit 5828578) on an arm64 mac with
# `:memory:` storage, using ``benchmark.pedantic(setup=...)`` so setup
# is excluded from timing. ~2.5× the observed median absorbs noise from
# cache / scheduler / GC variance without making the gate flappy. Lower
# these as the floor stabilises or when an optimisation lands.
LIMIT_INSERT_MANY_10K_MS = 1200.0  # observed median ~480 ms
LIMIT_UPDATE_MANY_5K_MS = 1900.0  # observed median ~744 ms
LIMIT_DELETE_MANY_5K_MS = 1400.0  # observed median ~564 ms
LIMIT_GROUP_4ACC_MS = 280.0  # observed median ~111 ms
LIMIT_FIND_INDEXED_RANGE_MS = 125.0  # observed median ~49 ms
LIMIT_SORT_MULTIKEY_MS = 630.0  # observed median ~253 ms

ROUNDS = 5
WARMUP = 1


@pytest.fixture
def coll():
    """Fresh server + fresh collection per test. Dropped on teardown.

    Server is ``port=0`` + ``:memory:`` so tests don't collide and leave
    no on-disk state.
    """
    server = SecantusDBServer(port=0, storage_path=":memory:")
    server.start()
    try:
        client = MongoClient(server.uri)
        try:
            yield client["perf"]["docs"]
        finally:
            client.close()
    finally:
        server.stop()


def _docs(n: int = 10000) -> list[dict]:
    return [{"_id": i, "i": i, "g": i % 50, "v": i * 2, "active": i % 2 == 0} for i in range(n)]


def _median_ms(benchmark) -> float:
    return benchmark.stats.stats.median * 1000.0


def test_insert_many_10k(coll, benchmark) -> None:
    docs = _docs(10000)

    def setup():
        coll.drop()
        return (), {}

    def workload():
        coll.insert_many(docs)

    benchmark.pedantic(workload, setup=setup, rounds=ROUNDS, warmup_rounds=WARMUP)
    actual = _median_ms(benchmark)
    assert actual < LIMIT_INSERT_MANY_10K_MS, (
        f"insert_many 10k regressed: median {actual:.1f} ms > limit {LIMIT_INSERT_MANY_10K_MS} ms"
    )


def test_update_many_5k_of_10k(coll, benchmark) -> None:
    docs = _docs(10000)

    def setup():
        coll.drop()
        coll.insert_many(docs)
        coll.create_index([("i", 1)])
        return (), {}

    def workload():
        coll.update_many({"i": {"$lt": 5000}}, {"$inc": {"v": 1}})

    benchmark.pedantic(workload, setup=setup, rounds=ROUNDS, warmup_rounds=WARMUP)
    actual = _median_ms(benchmark)
    assert actual < LIMIT_UPDATE_MANY_5K_MS, (
        f"update_many 5k regressed: median {actual:.1f} ms > limit {LIMIT_UPDATE_MANY_5K_MS} ms"
    )


def test_delete_many_5k_of_10k(coll, benchmark) -> None:
    docs = _docs(10000)

    def setup():
        coll.drop()
        coll.insert_many(docs)
        coll.create_index([("i", 1)])
        return (), {}

    def workload():
        coll.delete_many({"i": {"$lt": 5000}})

    benchmark.pedantic(workload, setup=setup, rounds=ROUNDS, warmup_rounds=WARMUP)
    actual = _median_ms(benchmark)
    assert actual < LIMIT_DELETE_MANY_5K_MS, (
        f"delete_many 5k regressed: median {actual:.1f} ms > limit {LIMIT_DELETE_MANY_5K_MS} ms"
    )


def test_aggregate_group_4_accumulators(coll, benchmark) -> None:
    docs = _docs(10000)
    coll.insert_many(docs)
    pipeline = [
        {
            "$group": {
                "_id": "$g",
                "total": {"$sum": "$v"},
                "avg": {"$avg": "$v"},
                "max": {"$max": "$v"},
                "min": {"$min": "$v"},
            }
        }
    ]

    def workload():
        list(coll.aggregate(pipeline))

    benchmark.pedantic(workload, rounds=ROUNDS, warmup_rounds=WARMUP)
    actual = _median_ms(benchmark)
    assert actual < LIMIT_GROUP_4ACC_MS, (
        f"$group 4-acc regressed: median {actual:.1f} ms > limit {LIMIT_GROUP_4ACC_MS} ms"
    )


def test_find_indexed_range(coll, benchmark) -> None:
    docs = _docs(10000)
    coll.insert_many(docs)
    coll.create_index([("v", 1)])

    def workload():
        list(coll.find({"v": {"$gte": 2000, "$lt": 8000}}))

    benchmark.pedantic(workload, rounds=ROUNDS, warmup_rounds=WARMUP)
    actual = _median_ms(benchmark)
    assert actual < LIMIT_FIND_INDEXED_RANGE_MS, (
        f"find indexed range regressed: median {actual:.1f} ms > "
        f"limit {LIMIT_FIND_INDEXED_RANGE_MS} ms"
    )


def test_aggregate_sort_multikey(coll, benchmark) -> None:
    docs = _docs(10000)
    coll.insert_many(docs)

    def workload():
        list(coll.aggregate([{"$sort": {"g": 1, "v": -1}}]))

    benchmark.pedantic(workload, rounds=ROUNDS, warmup_rounds=WARMUP)
    actual = _median_ms(benchmark)
    assert actual < LIMIT_SORT_MULTIKEY_MS, (
        f"$sort multi-key regressed: median {actual:.1f} ms > limit {LIMIT_SORT_MULTIKEY_MS} ms"
    )
