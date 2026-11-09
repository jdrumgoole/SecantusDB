"""A time-series `aggregate` read bucket by bucket (RM-348), against a live
mongod.

mongod reads a time-series collection's buckets through its cursor stage,
32 bucket documents first and twice as many at each refill, the bucket-level
filter deciding which are buffered, and unpacks each as the cursor reaches
it. Every scenario here has 50 series of 80 measurements one second apart,
so 100 buckets: 50 of 72 measurements in the first hour, 50 of 8 after it.
The cursor reads one, every measurement is deleted (or some are written),
and the rest is counted: what the buffer held. mongod orders buckets that
open in one window by their ObjectIds, which it generates, so the
measurements are counted, not compared in order.
"""

from __future__ import annotations

import datetime
import uuid
from collections.abc import Callable
from typing import Any

import pytest

BASE = datetime.datetime(2024, 1, 1)


def _at(seconds: int) -> datetime.datetime:
    return BASE + datetime.timedelta(seconds=seconds)


def _read(client: Any, pipeline: list, write: Callable[[Any], None]) -> tuple:
    db = client["rm348_live"]
    name = "ts" + uuid.uuid4().hex[:8]
    db.create_collection(name, timeseries={"timeField": "t", "metaField": "m"})
    c = db[name]
    c.insert_many([{"t": _at(i), "m": i % 50, "v": i} for i in range(4000)])
    cur = c.aggregate(pipeline, batchSize=1)
    first = next(cur)
    write(c)
    rest = list(cur)
    return len(rest), sorted(first), sorted({k for d in rest for k in d})


def _delete_all(c: Any) -> None:
    c.delete_many({"m": {"$gte": 0}})


def _write_late_and_early(c: Any) -> None:
    """A bucket in a window after every other, and measurements in the
    first window, after the buffer has passed it."""
    c.delete_many({"m": {"$gte": 0}})
    c.insert_many([{"t": _at(20000 + i), "m": 7, "v": -1} for i in range(5)])
    c.insert_many([{"t": _at(i), "m": 7, "v": -2} for i in range(5)])


MATCH = {"$match": {"v": {"$gte": 0}}}

SCENARIOS: dict[str, tuple[list, Callable[[Any], None]]] = {
    "a $match on a measurement": ([MATCH], _delete_all),
    "no stage": ([], _delete_all),
    "a filter that passes only the later buckets": (
        [{"$match": {"v": {"$gte": 3700}}}],
        _delete_all,
    ),
    "a filter on the time field": ([{"$match": {"t": {"$gte": _at(3600)}}}], _delete_all),
    "a filter few buckets pass": ([{"$match": {"v": {"$gt": 3990}}}], _delete_all),
    "then $addFields": ([MATCH, {"$addFields": {"w": 1}}], _delete_all),
    "then $unset": ([MATCH, {"$unset": "m"}], _delete_all),
    "then an exclusion $project": ([MATCH, {"$project": {"m": 0}}], _delete_all),
    "then $skip": ([MATCH, {"$skip": 1}], _delete_all),
    "then $limit": ([MATCH, {"$limit": 5000}], _delete_all),
    "a later bucket written, an earlier one passed": ([], _write_late_and_early),
}


@pytest.mark.mongod_live
@pytest.mark.parametrize("scenario", list(SCENARIOS))
def test_a_time_series_aggregate_buffers_buckets(
    rust_mongo: Any, mongod: Any, scenario: str
) -> None:
    pipeline, write = SCENARIOS[scenario]
    want = _read(mongod, pipeline, write)
    assert _read(rust_mongo, pipeline, write) == want
