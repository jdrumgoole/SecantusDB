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


@pytest.mark.backlog_fixed("RM-701")
def test_a_hinted_time_series_read_scans_the_index_over_the_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reads a time-series `aggregate` hinted to the default
    `m_1_t_1` index, by name or by key, by scanning that index over the
    buckets, buffering 32 of them series by series: 1,279. `{$natural: 1}`
    reads as no hint does: 2,303. This server read a hinted read whole,
    3,999, until a hint to the default index scanned it."""

    for hint, want in (("m_1_t_1", 1279), ({"m": 1, "t": 1}, 1279), ({"$natural": 1}, 2303)):

        def read(c: Any, hint: Any = hint) -> Any:
            return c.aggregate([{"$match": {"v": {"$gte": 0}}}], batchSize=1, hint=hint)

        assert _after_delete(mongod, read) == want
        assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-701")
def test_a_time_series_read_under_a_collation_buffers_its_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 reads a time-series `aggregate` under a collation
    through its cursor stage, 32 buckets first: 2,303 after the delete.
    This server read it whole, 3,999, until a collated read with no string
    bound streamed its buckets."""

    def read(c: Any) -> Any:
        return c.aggregate(
            [{"$match": {"v": {"$gte": 0}}}], batchSize=1, collation={"locale": "en"}
        )

    want = _after_delete(mongod, read)
    assert want == 2303
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-701")
def test_a_time_series_read_under_a_collation_matches_as_mongod(
    rust_mongo: Any, mongod: Any
) -> None:
    """A time-series read under a case-insensitive collation answers as
    mongod 8.2.11 does: a string bound matches under the collation, with
    no bucket pruned on its binary `control.min` / `control.max`."""

    def run(client: Any) -> Any:
        db = client["rm_found_1109_rm348"]
        name = "ts" + uuid.uuid4().hex[:8]
        db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
        c = db[name]
        base = datetime.datetime(2024, 1, 1)
        c.insert_many(
            [
                {"t": base + datetime.timedelta(hours=i), "m": i % 3, "s": s, "v": i}
                for i, s in enumerate(["abc", "ABC", "Abd", "b", "B", "a"])
            ]
        )
        out = []
        for match in ({"s": "abc"}, {"s": {"$gte": "B"}}, {"v": {"$gte": 2}}):
            got = c.aggregate([{"$match": match}], collation={"locale": "en", "strength": 2})
            out.append(sorted(d["v"] for d in got))
        return out

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog_fixed("RM-701")
def test_a_time_series_read_beside_an_index_of_its_own_buffers_its_buckets(
    rust_mongo: Any, mongod: Any
) -> None:
    """A time-series collection with an index of its own, `{v: 1}`: mongod
    8.2.11 still reads an unindexed filter's buckets through its cursor
    stage, 32 buckets first: 2,303 after the delete. This server read the
    collection whole at the command, 3,999, until a filter the index
    cannot serve streamed its buckets."""

    def read(c: Any) -> Any:
        c.create_index("v")
        return c.aggregate([{"$match": {"x": {"$exists": False}}}], batchSize=1)

    want = _after_delete(mongod, read)
    assert want == 2303
    assert _after_delete(rust_mongo, read) == want


@pytest.mark.backlog_fixed("RM-701")
def test_a_time_series_read_beside_an_index_of_its_own_matches_as_mongod(
    rust_mongo: Any, mongod: Any
) -> None:
    """With `{v: 1}` beside the default index, a read on `v`, on the meta
    field and on neither answers as mongod 8.2.11 does."""

    def run(client: Any) -> Any:
        db = client["rm_found_1109_rm348"]
        name = "ts" + uuid.uuid4().hex[:8]
        db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
        c = db[name]
        base = datetime.datetime(2024, 1, 1)
        c.insert_many(
            [
                {"t": base + datetime.timedelta(minutes=i), "m": i % 5, "v": i, "w": i % 7}
                for i in range(300)
            ]
        )
        c.create_index("v")
        out = []
        for match in ({"v": {"$gte": 250}}, {"m": 2, "w": 3}, {"w": {"$lt": 2}}):
            out.append(sorted(d["v"] for d in c.aggregate([{"$match": match}])))
        return out

    assert run(rust_mongo) == run(mongod)


@pytest.mark.backlog("RM-701")
def test_a_time_series_read_of_its_own_index_buffers_as_mongod(
    rust_mongo: Any, mongod: Any
) -> None:
    """With `{v: 1}` beside the default index, a filter on `v` that every
    measurement passes: mongod 8.2.11's planner picks the buckets in order
    over its index, 32 buckets first, 2,303 after the delete; this server
    reads any filter naming an own index's field whole, 3,999."""

    def read(c: Any) -> Any:
        c.create_index("v")
        return c.aggregate([{"$match": {"v": {"$gte": 0}}}], batchSize=1)

    want = _after_delete(mongod, read)
    assert want == 2303
    assert _after_delete(rust_mongo, read) == want
