"""Gap tests for the time-series bucket catalog's `serverStatus` section,
against mongod 8.2.11 (RM-549, split 2026-11-16 into three items).

The catalog's counters are cumulative for the life of the process, so each
test starts a server of its own on each side (the module-scoped fixtures
share counters with every other test) and a probe runs on both.
"""

from __future__ import annotations

import contextlib
import datetime as dt
import pathlib
from collections.abc import Iterator
from typing import Any

import pymongo
import pytest

from tests.backlog.conftest import rust_mongo_server

IDLE_THRESHOLD = "timeseriesIdleBucketExpiryMemoryUsageThreshold"


@contextlib.contextmanager
def _fresh_pair(store: pathlib.Path) -> Iterator[tuple[Any, Any]]:
    """A new `secantusd-mdb` and a new mongod: `(rust, mongod)` clients."""
    from tests.mongod_reference import MONGOD, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    with rust_mongo_server(store) as rust:
        proc, uri, dbpath = _start_mongod()
        ref = pymongo.MongoClient(uri, serverSelectionTimeoutMS=10_000)
        try:
            yield rust, ref
        finally:
            ref.close()
            _stop_mongod(proc, dbpath)


def _section(client: Any) -> dict:
    return client.admin.command("serverStatus").get("bucketCatalog", {})


def _make(client: Any) -> Any:
    db = client["rm_bucket_catalog"]
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    return db.ts


def _without_memory(section: dict) -> list:
    """The section in mongod's field order, less the one value (`memoryUsage`)
    that is RM-710's: the key stays, so a section that lacks it
    still differs."""
    return [(k, "<bytes>" if k == "memoryUsage" else v) for k, v in section.items()]


@pytest.mark.backlog("RM-709")
def test_bucket_catalog_counts_and_counters_follow_the_writes(tmp_path: pathlib.Path) -> None:
    """mongod 8.2.11, a fresh server: no section before the first write;
    after four inserts over two metadata values (`numBuckets` 2,
    `numBucketInserts` 2, `numCommits` 2, `numMeasurementsCommitted` 4,
    `avgNumMeasurementsPerCommit` 2, `stateManagement` `{bucketsManaged: 2,
    currentEra: 0, erasWithRemainingBuckets: 1, trackedClearOperations: 0}`),
    and after one more measurement into an open bucket (the bucket updated:
    `numBucketUpdates` 1, `numBuckets` still 2). Every field but
    `memoryUsage` is compared, in order."""
    t = dt.datetime(2026, 1, 1)

    def probe(client: Any) -> list:
        steps = [_section(client)]
        coll = _make(client)
        steps.append(_section(client))
        coll.insert_many([{"t": t, "m": i % 2} for i in range(4)])
        steps.append(_without_memory(_section(client)))
        coll.insert_one({"t": t, "m": 0})
        steps.append(_without_memory(_section(client)))
        return steps

    with _fresh_pair(tmp_path) as (rust, ref):
        got, want = probe(rust), probe(ref)
    assert want[0] == {} and want[1] == {}
    assert dict(want[2])["numBuckets"] == 2
    assert dict(want[2])["stateManagement"]["bucketsManaged"] == 2
    assert dict(want[3])["numBucketUpdates"] == 1
    assert got == want


@pytest.mark.backlog("RM-710")
def test_bucket_catalog_memory_usage_is_the_bytes_mongod_accounts(tmp_path: pathlib.Path) -> None:
    """mongod 8.2.11 charges the catalog's structures through tracking
    allocators: `memoryUsage` is 12552 with one bucket holding one
    measurement, and 12828 after four more measurements (a new `v` field
    among them) join that bucket; both repeat run for run. This server keeps
    no account."""
    t = dt.datetime(2026, 1, 1)

    def probe(client: Any) -> list:
        coll = _make(client)
        coll.insert_one({"t": t, "m": "a"})
        first = _section(client).get("memoryUsage")
        coll.insert_many(
            [{"t": t + dt.timedelta(seconds=i), "m": "a", "v": i} for i in range(1, 5)]
        )
        return [first, _section(client).get("memoryUsage")]

    with _fresh_pair(tmp_path) as (rust, ref):
        got, want = probe(rust), probe(ref)
    assert want == [12552, 12828]
    assert got == want


@pytest.mark.backlog("RM-711")
def test_idle_buckets_expire_past_the_memory_threshold(tmp_path: pathlib.Path) -> None:
    """mongod 8.2.11: once the catalog's memory passes
    `timeseriesIdleBucketExpiryMemoryUsageThreshold` (set to 30000 bytes after
    the first write), opening a bucket for a new metadata value closes idle
    ones: of 21 buckets opened for 21 metadata values the catalog holds fewer
    (14 to 17 across runs: which ones expire depends on the stripe each lands
    in), `numBucketsClosedDueToMemoryThreshold` equals
    `numBucketsArchivedDueToMemoryThreshold` and is at least 1, and every
    bucket document stays stored. Asserted as those facts, not the counts.
    Needs RM-710 (the accounting) to decide when the threshold is passed."""
    t = dt.datetime(2026, 1, 1)

    def probe(client: Any) -> dict:
        coll = _make(client)
        coll.insert_one({"t": t, "m": -1})
        try:
            client.admin.command("setParameter", 1, **{IDLE_THRESHOLD: 30000})
        except pymongo.errors.OperationFailure as e:
            return {"setParameter": ("err", e.code)}
        for i in range(20):
            coll.insert_one({"t": t, "m": i})
        s = _section(client)
        closed = s.get("numBucketsClosedDueToMemoryThreshold", 0)
        archived = s.get("numBucketsArchivedDueToMemoryThreshold", 0)
        return {
            "fewer_buckets_than_opened": s.get("numBuckets", 21) < 21,
            "expired": closed >= 1,
            "closed_equals_archived": closed == archived,
            "all_documents_stored": client["rm_bucket_catalog"][
                "system.buckets.ts"
            ].count_documents({})
            == 21,
        }

    with _fresh_pair(tmp_path) as (rust, ref):
        got, want = probe(rust), probe(ref)
    assert want == {
        "fewer_buckets_than_opened": True,
        "expired": True,
        "closed_equals_archived": True,
        "all_documents_stored": True,
    }
    assert got == want
