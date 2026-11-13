"""Gap tests for what working RM-325 left open (2026-11-13): the spill
engine's sizes once it writes pages, the SBE `$group`'s spill, a `$median`'s
spilled state and the spill engine's `serverStatus` section, each measured
against mongod 8.2.11.

Every test asks mongod live and compares; it fails while its item is open,
on an assertion.
"""

from __future__ import annotations

import contextlib
from collections.abc import Iterator
from typing import Any

import pymongo
import pytest

from tests.backlog.conftest import rust_mongo_server

DEFAULT = 104_857_600


def _window_storage(client: Any) -> int:
    """A `$setWindowFields` spill of 28 MB: 3000 documents of 10000 bytes in
    seven partitions, a whole-partition `$sum`, under a 500000-byte budget."""
    client.drop_database("rm325big")
    db = client.rm325big
    db.c.insert_many([{"_id": i, "k": i % 7, "s": "x" * 10_000, "n": i} for i in range(3000)])
    client.admin.command(
        {"setParameter": 1, "internalDocumentSourceSetWindowFieldsMaxMemoryBytes": 500_000}
    )
    try:
        explained = db.command(
            "explain",
            {
                "aggregate": "c",
                "pipeline": [
                    {"$setWindowFields": {"partitionBy": "$k", "output": {"t": {"$sum": "$n"}}}}
                ],
                "cursor": {},
            },
            verbosity="executionStats",
        )
    finally:
        client.admin.command(
            {"setParameter": 1, "internalDocumentSourceSetWindowFieldsMaxMemoryBytes": DEFAULT}
        )
    stage = next(s for s in explained["stages"] if "$_internalSetWindowFields" in s)
    return stage["spilledDataStorageSize"]


@pytest.mark.backlog("RM-611")
def test_a_window_spill_the_engine_writes_out_is_mongods_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's spill engine writes part of a 28 MB window spill to its
    file, 73728 bytes every run (zstd-compressed pages; 4096 at 14 MB, 204800
    at 60 MB). This server's uncompressed table leaves the file at 4096 or
    writes megabytes, as its eviction threads happen to run."""
    assert _window_storage(rust_mongo) == _window_storage(mongod)


SBE_BUDGET = "internalQuerySlotBasedExecutionHashAggApproxMemoryUseInBytesBeforeSpill=50000"


def _sbe_group_spill(client: Any) -> dict:
    client.drop_database("rm325sbe")
    db = client.rm325sbe
    db.c.insert_many([{"_id": i, "k": i % 7, "s": "x" * 1000, "n": i} for i in range(300)])
    db.command({"profile": 2})
    try:
        list(db.c.aggregate([{"$group": {"_id": "$n", "t": {"$push": "$s"}}}], comment="sbe"))
    finally:
        db.command({"profile": 0})
    entry = db.system.profile.find_one({"command.comment": "sbe"}) or {}
    return {k: v for k, v in entry.items() if k.startswith("group") or k == "usedDisk"}


@contextlib.contextmanager
def _mongod_with(*extra: str) -> Iterator[Any]:
    from tests.mongod_reference import MONGOD, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod(extra_args=extra)
    client = pymongo.MongoClient(uri, serverSelectionTimeoutMS=10_000)
    try:
        yield client
    finally:
        client.close()
        _stop_mongod(proc, dbpath)


@pytest.mark.backlog("RM-612")
def test_an_sbe_group_spills_as_mongods(tmp_path: Any) -> None:
    """mongod 8.2.11 started with the SBE `$group`'s budget at 50000 spills a
    `$push` of 300 1000-byte strings 7 times, 312300 bytes, into its spill
    engine (4096 on disk). This server refuses the startup parameter."""
    with _mongod_with("--setParameter", SBE_BUDGET) as mongod:
        expected = _sbe_group_spill(mongod)
    with rust_mongo_server(tmp_path, "--setParameter", SBE_BUDGET) as rust:
        assert _sbe_group_spill(rust) == expected


def _median_spilled_bytes(client: Any) -> Any:
    client.drop_database("rm325median")
    db = client.rm325median
    db.c.insert_many([{"_id": i, "k": i % 7, "s": "x" * 1000, "n": i} for i in range(300)])
    client.admin.command({"setParameter": 1, "internalDocumentSourceGroupMaxMemoryBytes": 5000})
    db.command({"profile": 2})
    try:
        list(
            db.c.aggregate(
                [
                    {"$unwind": "$s"},
                    {
                        "$group": {
                            "_id": "$n",
                            "m": {"$median": {"input": "$n", "method": "approximate"}},
                        }
                    },
                ],
                comment="median",
            )
        )
    finally:
        db.command({"profile": 0})
        client.admin.command(
            {"setParameter": 1, "internalDocumentSourceGroupMaxMemoryBytes": DEFAULT}
        )
    entry = db.system.profile.find_one({"command.comment": "median"}) or {}
    return (entry.get("groupSpilledRecords"), entry.get("groupSpilledBytes"))


@pytest.mark.backlog("RM-613")
def test_a_median_spills_mongods_state(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's classic `$group` spills each group's `$median` as its
    t-digest: 300 groups of one value each, 84000 bytes. This server spills
    the values it keeps, 100800."""
    assert _median_spilled_bytes(rust_mongo) == _median_spilled_bytes(mongod)


def _spill_engine_status(client: Any) -> Any:
    section = client.admin.command("serverStatus").get("spillWiredTiger")
    if section is None:
        return None
    return {"storageSize": section.get("storageSize"), "uri": section.get("uri")}


@pytest.mark.backlog("RM-614")
def test_server_status_reports_the_spill_engine(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `serverStatus` has a `spillWiredTiger` section from
    startup -- the spill engine's statistics, `storageSize` 0 while nothing
    is spilled (`spillWiredTigerServerStatusVerbosity` 1). This server's has
    none."""
    assert _spill_engine_status(rust_mongo) == _spill_engine_status(mongod)
