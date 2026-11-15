"""Gap tests for items found while working RM-088 (stored time-series
buckets), 2026-11-11, against mongod 8.2.11."""

from __future__ import annotations

import datetime as dt
from typing import Any

import pytest


def _both(rust: Any, mongod: Any, probe: Any) -> tuple:
    return probe(rust), probe(mongod)


@pytest.mark.backlog_fixed("RM-548")
def test_bucket_indexes_list_a_ttl_index_as_mongod_keeps_it(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps a TTL index on a time-series collection's time
    field as `{control.min.<time>: 1}` on the buckets, its partial filter on
    the meta field as one on `meta` (`{meta: {$eq: ...}}`), and the index as
    asked in `originalSpec`; `listIndexes` of `system.buckets.<name>` shows
    that. This server showed the bound pair and the filter as given."""

    def probe(client: Any) -> list:
        client.drop_database("rm_found_ts_idx")
        db = client["rm_found_ts_idx"]
        db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
        db.ts.create_index([("t", 1)], expireAfterSeconds=3600, partialFilterExpression={"m": "a"})
        return [
            {k: v for k, v in ix.items() if k != "v"}
            for ix in db["system.buckets.ts"].list_indexes()
            if ix["name"] == "t_1"
        ]

    got, ref = _both(rust_mongo, mongod, probe)
    assert ref[0]["key"] == {"control.min.t": 1}
    assert got == ref


@pytest.mark.backlog_fixed("RM-548")
@pytest.mark.parametrize(
    ("key", "options"),
    [
        ([("t", 1)], {}),
        ([("t", -1)], {"partialFilterExpression": {"m": "a"}, "expireAfterSeconds": 10}),
        ([("m", 1), ("t", 1)], {"partialFilterExpression": {"m": "a"}}),
        ([("m", 1)], {"partialFilterExpression": {"m": {"$gt": 1}}}),
        ([("m", 1)], {"partialFilterExpression": {"m.k": {"$exists": True}}}),
        ([("m", 1)], {"partialFilterExpression": {"m": "a", "x": {"$gte": 2}}}),
        ([("m", 1)], {"partialFilterExpression": {"x": 3}}),
        ([("x", 1)], {"partialFilterExpression": {"x": {"$gt": 5}}}),
        ([("m", 1)], {"partialFilterExpression": {"t": {"$gt": dt.datetime(2026, 1, 1)}}}),
        ([("m", 1)], {"sparse": True}),
    ],
)
def test_bucket_indexes_list_each_index_as_mongod_keeps_it(
    rust_mongo: Any, mongod: Any, key: list, options: dict
) -> None:
    """Each kind of index on a time-series collection, listed on its buckets
    as mongod 8.2.11 lists it: the key, the partial filter translated to
    bucket predicates, the other options and, for a partial index, the spec
    as asked in `originalSpec` (RM-548); and on the collection itself, with
    no `originalSpec`."""

    def probe(client: Any) -> list:
        client.drop_database("rm_548_ts_idx")
        db = client["rm_548_ts_idx"]
        db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
        db.ts.create_index(key, name="ix", **options)
        on_buckets = [ix for ix in db["system.buckets.ts"].list_indexes() if ix["name"] == "ix"]
        return on_buckets + [ix for ix in db.ts.list_indexes() if ix["name"] == "ix"]

    got, ref = _both(rust_mongo, mongod, probe)
    assert got == ref


@pytest.mark.backlog("RM-549")
def test_server_status_reports_the_bucket_catalog(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `serverStatus` has a `bucketCatalog` section once a
    time-series collection has been written to: the buckets it holds open,
    idle and archived, its memory use, the execution counters and its state
    registry. This server reports none."""

    def probe(client: Any) -> list:
        client.drop_database("rm_found_ts_mem")
        db = client["rm_found_ts_mem"]
        db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
        db.ts.insert_many([{"t": dt.datetime(2026, 1, 1), "m": i % 2} for i in range(4)])
        section = client.admin.command("serverStatus").get("bucketCatalog", {})
        return list(section.keys())

    got, ref = _both(rust_mongo, mongod, probe)
    assert "memoryUsage" in ref
    assert got == ref
