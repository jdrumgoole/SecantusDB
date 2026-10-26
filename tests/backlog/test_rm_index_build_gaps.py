"""Gaps found while building the hybrid index build (RM-187), 2026-10-26."""

from __future__ import annotations

import time
from typing import Any

import pymongo
import pytest

from tests.backlog.conftest import mongod_rs, rust_mongo  # noqa: F401  (fixtures)
from tests.mongod_live.test_index_builds import _Build, mongod_fp, rust_fp  # noqa: F401


def _write_after_catalog_change(client: Any) -> tuple:
    db = client.rm320
    client.drop_database("rm320")
    db.c.insert_many([{"_id": i, "a": i} for i in range(10)])
    db.other.insert_one({"_id": 1})
    with client.start_session() as s:
        s.start_transaction()
        # The transaction's snapshot is taken here, on another collection.
        db.other.find_one({}, session=s)
        db.command("createIndexes", "c", indexes=[{"key": {"a": 1}, "name": "a_1"}])
        try:
            db.c.insert_one({"_id": 100, "a": 100}, session=s)
            s.commit_transaction()
            outcome: tuple = ("committed",)
        except pymongo.errors.OperationFailure as e:
            outcome = ("err", e.code, sorted((e.details or {}).get("errorLabels", [])))
            s.abort_transaction()
    found = [d["_id"] for d in db.c.find({"a": {"$gte": 0}}).hint("a_1")]
    client.drop_database("rm320")
    return outcome, found


@pytest.mark.backlog("RM-320")
def test_a_transaction_older_than_an_index_cannot_write_past_it(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11: a transaction whose snapshot predates an index cannot
    write to its collection once the index commits -- 112 WriteConflict
    ("Unable to write to collection ... due to catalog changes"), labelled
    TransientTransactionError. This server commits the write without the
    new index's entry, so the index misses the document."""
    theirs = _write_after_catalog_change(mongod_rs)
    assert theirs == (("err", 112, ["TransientTransactionError"]), list(range(10))), theirs
    assert _write_after_catalog_change(rust_mongo) == theirs


def _stats_while_building(client: Any) -> tuple:
    client.drop_database("rm321")
    db = client.rm321
    db.c.insert_many([{"_id": i, "a": i} for i in range(100)])
    build = _Build(
        client,
        "rm321",
        [{"key": {"a": 1}, "name": "a_1"}],
        "hangIndexBuildBeforeCommit",
    )
    try:
        # Held before its commit: the build has recorded itself ready.
        deadline = time.monotonic() + 30
        while not client.config["system.indexBuilds"].find_one(
            {"commitReadyMembers": {"$exists": True}}
        ):
            assert time.monotonic() < deadline, "the build never got ready to commit"
            time.sleep(0.05)
        stats = db.command("collStats", "c")
        index_stats = sorted(
            (s["name"], s.get("building", False)) for s in db.c.aggregate([{"$indexStats": {}}])
        )
        out = (stats["nindexes"], sorted(stats["indexSizes"]), index_stats)
    finally:
        build.finish()
    client.drop_database("rm321")
    return out


@pytest.mark.backlog("RM-321")
def test_a_building_index_counts_in_stats(rust_fp: Any, mongod_fp: Any) -> None:  # noqa: F811
    """mongod 8.2.11 counts an index still being built in `collStats`
    (`nindexes`, `indexSizes`) and lists it in `$indexStats` with
    `building: true`; this server leaves it out of both until it commits."""
    theirs = _stats_while_building(mongod_fp)
    assert theirs == (2, ["_id_", "a_1"], [("_id_", False), ("a_1", True)]), theirs
    assert _stats_while_building(rust_fp) == theirs
