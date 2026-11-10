"""Gap tests for what working RM-215 (the query stats store, 2026-11-10)
found: the parts of `$queryStats` the store does not yet serve, and the
date-part operators' argument written as an array of one.

Each query stats test runs on servers of its own: what a store records
outlives the parameter that let it record, and would show in every later
`$queryStats` on a shared server.
"""

from __future__ import annotations

import datetime
from collections.abc import Iterator
from typing import Any

import pymongo
import pytest
from bson import Binary

from tests.backlog.conftest import mongo_answer, rust_mongo_server


@pytest.fixture
def own_servers(tmp_path: Any) -> Iterator[tuple[Any, Any]]:
    from tests.mongod_reference import MONGOD, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod()
    reference = pymongo.MongoClient(uri, serverSelectionTimeoutMS=10_000)
    try:
        with rust_mongo_server(tmp_path / "store") as ours:
            yield ours, reference
    finally:
        reference.close()
        _stop_mongod(proc, dbpath)


def _remainder(client: Any) -> Any:
    db = client.rm215r
    db.c.insert_many([{"a": i} for i in range(3)])
    size = mongo_answer(
        lambda: client.admin.command("getParameter", 1, internalQueryStatsCacheSize=1)[
            "internalQueryStatsCacheSize"
        ]
    )
    client.admin.command("setParameter", 1, internalQueryStatsRateLimit=-1)
    list(db.c.find({"a": {"$gt": 1}}))
    db.command("aggregate", "c", pipeline=[{"$match": {"a": 1}}], explain=True)
    explained = [
        e["key"].get("explain")
        for e in client.admin.aggregate([{"$queryStats": {}}])
        if e["key"]["queryShape"]["cmdNs"].get("db") == "rm215r"
    ]
    hmac = {"algorithm": "hmac-sha-256", "hmacKey": Binary(b"0123456789abcdef0123456789abcdef", 8)}
    transformed = mongo_answer(
        lambda: sorted(
            str(e["key"]["queryShape"])
            for e in client.admin.aggregate([{"$queryStats": {"transformIdentifiers": hmac}}])
            if e["key"]["queryShape"].get("command") == "find"
        )
    )
    return size, sorted(str(x) for x in explained), transformed


@pytest.mark.backlog("RM-532")
def test_query_stats_remainder(own_servers: Any) -> None:
    """mongod 8.2.11 reports `internalQueryStatsCacheSize` ("1%"), records an
    `aggregate` that explains (its key says `explain:
    "queryPlanner"`), and lists its entries with every identifier as its
    HMAC under `transformIdentifiers`. This server reports no cache size,
    records no explain, and refuses `transformIdentifiers` (238) once its
    store has entries."""
    ours, theirs = own_servers
    assert _remainder(ours) == _remainder(theirs)


def _date_parts(client: Any) -> Any:
    client.drop_database("rm533")
    db = client.rm533
    db.c.insert_many([{"_id": 0}, {"_id": 1, "t": datetime.datetime(2020, 1, 9)}])
    return [
        mongo_answer(lambda e=e: list(db.c.aggregate([{"$project": {"r": e}}])))
        for e in [
            {"$isoWeek": ["$t"]},
            {"$year": ["$t"]},
            {"$dayOfWeek": ["$t"]},
            {"$isoWeek": [{"date": "$t"}]},
            {"$hour": ["$t", "$t"]},
        ]
    ]


@pytest.mark.backlog("RM-533")
def test_a_date_part_takes_an_array_of_one(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 takes a date-part operator's argument written as an
    array of one (`{$year: ["$t"]}`) as that one expression: null for a
    missing field, the part of a date. This server evaluates the array
    itself and fails with 16006 "can't convert from BSON type array to
    Date"."""
    assert _date_parts(rust_mongo) == _date_parts(mongod)
