"""Gap tests for what was found on 2026-11-09 while reading a time-series
`aggregate` bucket by bucket (RM-348): the time-series reads that still
read the collection at the command.

Each reads 4,000 measurements in 50 series, 100 buckets (50 of 72 in the
first hour, 50 of 8 after it), takes one, deletes every measurement, and
counts what the cursor still returns: what mongod's read had buffered.

The reference is mongod 8.2.11 (`mongod`); the Rust server is
`secantusd-mdb` (`rust_mongo`).
"""

from __future__ import annotations

import datetime
import uuid
from collections.abc import Callable
from typing import Any

import pytest


def _after_delete(client: Any, read: Callable[[Any], Any]) -> int:
    db = client["rm_found_1109_rm348"]
    name = "ts" + uuid.uuid4().hex[:8]
    db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
    c = db[name]
    base = datetime.datetime(2024, 1, 1)
    c.insert_many(
        [{"t": base + datetime.timedelta(seconds=i), "m": i % 50, "v": i} for i in range(4000)]
    )
    cur = read(c)
    next(cur)
    c.delete_many({"m": {"$gte": 0}})
    return len(list(cur))


@pytest.mark.backlog_fixed("RM-455")
def test_a_time_series_find_goes_bucket_by_bucket(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers a `find` of a time-series collection with an
    aggregation whose cursor stage buffers 32 buckets first: 2,303 more
    after the delete. This server read the collection at the command, and
    returned 3,999, until its `find` became that aggregation too."""

    def read(c: Any) -> Any:
        return c.find({"v": {"$gte": 0}}).batch_size(1)

    want = _after_delete(mongod, read)
    assert want == 2303
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-456")
def test_a_time_series_projection_reads_a_bucket_at_a_time(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reads a time-series `aggregate` behind an inclusion
    `$project` one bucket at a time: the rest of the first bucket, 71.
    This server read it whole, 3,999, until such a pipeline read its
    buckets one at a time with no cursor-stage buffer."""

    def read(c: Any) -> Any:
        return c.aggregate([{"$match": {"v": {"$gte": 0}}}, {"$project": {"v": 1}}], batchSize=1)

    want = _after_delete(mongod, read)
    assert want == 71
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-457")
def test_a_time_series_read_of_the_meta_index_buffers_its_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reads a filter on the meta field from the default
    `m_1_t_1` index, buffering 32 buckets series by series: 1,279. This
    server read it whole, 3,199, until that index was built over the
    buckets and the read scanned it."""

    def read(c: Any) -> Any:
        return c.aggregate([{"$match": {"m": {"$lt": 40}}}], batchSize=1)

    want = _after_delete(mongod, read)
    assert want == 1279
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog("RM-701")
def test_a_hinted_time_series_read_scans_the_index_over_the_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reads a time-series `aggregate` hinted to the default
    `m_1_t_1` index by scanning that index over the buckets, buffering 32
    of them series by series; this server reads a hinted read whole."""

    def read(c: Any) -> Any:
        return c.aggregate([{"$match": {"v": {"$gte": 0}}}], batchSize=1, hint="m_1_t_1")

    want = _after_delete(mongod, read)
    assert want == 1279
    assert _after_delete(rust_mongo, read) == want
