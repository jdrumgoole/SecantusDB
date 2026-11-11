"""Stored time-series buckets against mongod 8.2 (RM-088).

The Rust MongoDB server keeps a time-series collection as mongod does: bucket
documents in `system.buckets.<name>`, placed by mongod's bucket catalog
(`crates/secantus-commands/src/bucket_catalog.rs`). Each test runs the same
writes on both servers and compares what is stored and what is read back.

Two things are normalised, because mongod's own answer is not stable: the
order of a bucket's `data` columns (mongod keeps them in a hash map seeded
per process; three runs of one insert gave three orders, 2026-11-11), and
so the order of an unpacked measurement's fields after time and meta. A
bucket's `_id` is mongod's to mint (its time and a random counter) and is
masked.
"""

from __future__ import annotations

import datetime as dt
import time
from typing import Any

import pytest

from tests.mongod_live.conftest import same

T0 = dt.datetime(2026, 1, 1)


def at(minutes: float) -> dt.datetime:
    return T0 + dt.timedelta(minutes=minutes)


def fresh(client: Any, name: str) -> Any:
    client.drop_database(name)
    return client[name]


def buckets(db: Any, coll: str = "ts", rewritten: bool = False) -> list:
    """The stored buckets, `_id` masked and the columns in name order.

    `rewritten`: the summaries too. mongod rebuilds a bucket it rewrites
    (a delete) from its unpacked measurements, whose fields after time
    and meta come in its columns' hash order, so the summaries of a
    rewritten bucket are in that order too."""
    out = []
    for b in db[f"system.buckets.{coll}"].find():
        b = dict(b)
        b["_id"] = "masked"
        b["data"] = dict(sorted(b["data"].items()))
        if rewritten:
            control = dict(b["control"])
            for bound in ("min", "max"):
                control[bound] = dict(sorted(control[bound].items()))
            b["control"] = control
        out.append(b)
    return out


def counters(db: Any, coll: str = "ts") -> dict:
    stats = db.command("collStats", coll)["timeseries"]
    return {k: v for k, v in stats.items() if k.startswith("num") or k.startswith("avgNum")}


def write(client: Any, name: str, ts: dict, batches: list[list[dict]]) -> Any:
    db = fresh(client, name)
    db.create_collection("ts", timeseries=ts)
    n = 0
    for batch in batches:
        docs = []
        for d in batch:
            d = dict(d)
            d.setdefault("_id", 100_000 + n)
            n += 1
            docs.append(d)
        db.ts.insert_many(docs)
    return db


SCENARIOS = {
    # control.min / control.max take each field where the measurements
    # first have it, the time field included.
    "field_order": (
        {"timeField": "t", "metaField": "m"},
        [
            [{"_id": 1, "v": 2, "m": {"b": 1, "a": 2}, "t": at(0), "s": "x"}],
            [{"t": at(2), "_id": 3, "m": {"a": 2, "b": 1}, "v": 3.5, "o": {"k": [1, 2]}}],
        ],
    ),
    # Time running backwards archives the bucket and opens another.
    "backwards": (
        {"timeField": "t"},
        [[{"t": at(5), "v": 1}], [{"t": at(1), "v": 2}], [{"t": at(3), "v": 3}]],
    ),
    "nested_min_max": (
        {"timeField": "t"},
        [[{"t": at(0), "o": {"x": 5, "y": [3, 1]}}, {"t": at(1), "o": {"y": [0, 9, 2], "z": "q"}}]],
    ),
    "series_in_one_batch": (
        {"timeField": "t", "metaField": "m"},
        [[{"t": at(i), "v": i, "m": i % 2} for i in range(6)]],
    ),
    # Large values close a bucket for size; one of fewer than ten
    # measurements is kept open for them first.
    "closed_for_size": (
        {"timeField": "t", "metaField": "m"},
        [[{"t": at(i / 60), "m": 1, "blob": "x" * 20_000} for i in range(12)]],
    ),
    # Every commit updates the one bucket.
    "small_batches": (
        {"timeField": "t", "metaField": "m"},
        [[{"t": at(i / 600), "m": 1, "v": i, "w": "abcdefghijklmnopqrstuvwxyz" * 4}] for i in range(120)],
    ),
    # A measurement earlier than the last one written makes the bucket
    # unsorted (control.version 3).
    "unsorted": (
        {"timeField": "t", "metaField": "m"},
        [[{"t": at(2), "m": 1}], [{"t": at(1), "m": 1}]],
    ),
}


@pytest.mark.parametrize("label", sorted(SCENARIOS))
def test_buckets_are_stored_as_mongod_stores_them(
    rust_mongo: Any, mongod: Any, label: str
) -> None:
    ts, batches = SCENARIOS[label]

    def run(client: Any) -> tuple:
        db = write(client, f"tsb_{label}", ts, batches)
        measurements = [sorted(m.items()) for m in db.ts.find({}, {"_id": 0})]
        return buckets(db), measurements, counters(db)

    same(run(rust_mongo), run(mongod))


def test_deletes_and_meta_updates_rewrite_the_buckets(rust_mongo: Any, mongod: Any) -> None:
    def run(client: Any) -> tuple:
        db = fresh(client, "tsb_writes")
        db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
        db.ts.insert_many([{"_id": i, "t": at(i), "m": i % 2, "v": i} for i in range(6)])
        deleted = db.ts.delete_many({"v": {"$in": [1, 2]}}).deleted_count
        one = db.ts.delete_one({"m": 1}).deleted_count
        updated = db.ts.update_many({"m": 0}, {"$set": {"m": 5}})
        # A measurement's fields after time and meta: the columns' order.
        removed = sorted(db.ts.find_one_and_delete({"v": 4}).items())
        after = buckets(db, rewritten=True)
        # The catalog forgot the buckets it did not write: the next
        # measurement reopens one.
        db.ts.insert_one({"_id": 9, "t": at(9), "m": 5})
        return (
            deleted,
            one,
            updated.matched_count,
            updated.modified_count,
            removed,
            after,
            buckets(db, rewritten=True),
            [d["_id"] for d in db.ts.find()],
        )

    same(run(rust_mongo), run(mongod))


def test_a_bucket_written_by_hand(rust_mongo: Any, mongod: Any) -> None:
    def run(client: Any) -> tuple:
        db = fresh(client, "tsb_direct")
        db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
        db.ts.insert_many([{"_id": i, "t": at(i), "m": 1} for i in range(3)])
        changed = db["system.buckets.ts"].update_many({}, {"$set": {"meta": 2}}).modified_count
        db.ts.insert_one({"_id": 7, "t": at(5), "m": 1})
        placed = sorted((b["meta"], b["control"]["count"]) for b in db["system.buckets.ts"].find())
        gone = db["system.buckets.ts"].delete_many({"meta": 2}).deleted_count
        return changed, placed, gone, db.ts.count_documents({})

    same(run(rust_mongo), run(mongod))


def test_ttl_expires_whole_buckets(rust_mongo: Any, mongod: Any) -> None:
    """A bucket expires once the newest measurement it could hold has:
    `expireAfterSeconds` and then `bucketMaxSpanSeconds` before now."""
    now = dt.datetime.now(dt.timezone.utc).replace(tzinfo=None, microsecond=0)

    def run(client: Any) -> tuple:
        client.admin.command("setParameter", 1, ttlMonitorSleepSecs=1)
        try:
            db = fresh(client, "tsb_ttl")
            db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
            db.ts.create_index(
                [("t", 1)], expireAfterSeconds=3600, partialFilterExpression={"m": "a"}
            )
            for hours, meta in [(3, "a"), (1.5, "a"), (2.5, "a"), (3, "b"), (0, "a")]:
                db.ts.insert_one({"t": now - dt.timedelta(hours=hours), "m": meta, "h": hours})
            db.create_collection("c", timeseries={"timeField": "t"}, expireAfterSeconds=3600)
            for hours in [3, 2.5, 1.5, 0]:
                db.c.insert_one({"t": now - dt.timedelta(hours=hours), "h": hours})
            # Expired is 3 left in `ts` and 2 in `c`; wait for that or time out.
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if db.ts.count_documents({}) == 3 and db.c.count_documents({}) == 2:
                    break
                time.sleep(0.25)
            return (
                sorted((d["m"], d["h"]) for d in db.ts.find()),
                sorted(d["h"] for d in db.c.find()),
            )
        finally:
            client.admin.command("setParameter", 1, ttlMonitorSleepSecs=60)

    same(run(rust_mongo), run(mongod))


def test_db_stats_counts_the_buckets(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counts a time-series collection as a view, its bucket
    collection as a collection, and both definitions in `system.views`."""

    def run(client: Any) -> dict:
        db = fresh(client, "tsb_dbstats")
        db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
        db.create_collection("v", viewOn="ts", pipeline=[])
        db.ts.insert_many([{"t": T0, "m": i % 2, "v": i} for i in range(4)])
        stats = db.command("dbStats")
        return {k: stats[k] for k in ("collections", "views", "objects", "indexes")}

    same(run(rust_mongo), run(mongod))


def test_a_clustered_collection_reads_in_id_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod keys a clustered collection's records by `_id`: an unsorted
    read returns them in `_id` order, not the order they were written."""

    def run(client: Any) -> tuple:
        db = fresh(client, "tsb_clustered")
        db.create_collection("c", clusteredIndex={"key": {"_id": 1}, "unique": True})
        for i in [3, 1, 2, 5, 4]:
            db.c.insert_one({"_id": i})
        found = [d["_id"] for d in db.c.find()]
        aggregated = [d["_id"] for d in db.c.aggregate([])]
        return found, aggregated

    same(run(rust_mongo), run(mongod))
