"""RM-720: a classic `$group`'s approximate `$median` / `$percentile` that
spills, against mongod 8.2.11. mongod counts its t-digest's buffer AND the
centroids each flush leaves (`TDigest::memUsageBytes`), spills the digest's
merged centroids (`TDigestDistributedClassic::serialize`) and merges them
back as digests (`combine`), so where it spills and the answer it gives
both follow the digest, not the values.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import only_on

DEFAULT = 104_857_600


def _spilled_medians(client: Any, budget: int = 150_000) -> tuple:
    client.drop_database("rm720spill")
    db = client.rm720spill
    # Three groups, interleaved, of 15000 values each: every group's buffer
    # flushes twice, so its digest holds merged centroids when it spills.
    db.c.insert_many([{"k": i % 3, "n": (i * 7919) % 30011 * 0.37} for i in range(45_000)])
    pipeline = [
        {"$sort": {"_id": 1}},
        {
            "$group": {
                "_id": "$k",
                "m": {"$median": {"input": "$n", "method": "approximate"}},
                "p": {"$percentile": {"input": "$n", "p": [0.1, 0.9], "method": "approximate"}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    client.admin.command({"setParameter": 1, "internalDocumentSourceGroupMaxMemoryBytes": budget})
    db.command({"profile": 2})
    try:
        rows = list(db.c.aggregate(pipeline, allowDiskUse=True, comment="rm720"))
    finally:
        db.command({"profile": 0})
        client.admin.command(
            {"setParameter": 1, "internalDocumentSourceGroupMaxMemoryBytes": DEFAULT}
        )
    entry = db.system.profile.find_one({"command.comment": "rm720"}) or {}
    return (
        [(r["_id"], r["m"], r["p"]) for r in rows],
        entry.get("groupSpills"),
        entry.get("groupSpilledRecords"),
    )


def _one_group(client: Any) -> tuple:
    client.drop_database("rm720one")
    db = client.rm720one
    db.c.insert_many([{"n": (i * 7919) % 30011 * 0.37} for i in range(30_000)])
    group = {"_id": None, "m": {"$median": {"input": "$n", "method": "approximate"}}}
    client.admin.command({"setParameter": 1, "internalDocumentSourceGroupMaxMemoryBytes": 70_000})
    db.command({"profile": 2})
    try:
        (row,) = db.c.aggregate(
            [{"$sort": {"_id": 1}}, {"$group": group}], allowDiskUse=True, comment="rm720one"
        )
    finally:
        db.command({"profile": 0})
        client.admin.command(
            {"setParameter": 1, "internalDocumentSourceGroupMaxMemoryBytes": DEFAULT}
        )
    entry = db.system.profile.find_one({"command.comment": "rm720one"}) or {}
    return row["m"], entry.get("groupSpills"), entry.get("groupSpilledRecords")


@pytest.mark.backlog_fixed("RM-720")
def test_a_median_counts_its_centroids_and_spills_them(rust_mongo: Any, mongod: Any) -> None:
    """One group of 30000 values under a 70000-byte budget: mongod 8.2.11
    spills it 5 times, because the centroids each flush leaves are counted
    on top of the 8192-double buffer, and answers 5551.9832 from the merged
    digests. This server counted the buffer alone, never spilled, and
    answered 5552.178172875341."""
    assert _one_group(rust_mongo) == _one_group(mongod)


@pytest.mark.backlog_fixed("RM-720")
def test_a_spilled_median_is_mongods_merged_tdigest(rust_mongo: Any, mongod: Any) -> None:
    """This server spilled every value as a weight-one centroid and rebuilt
    the digest from them sorted, so a spilled group's approximate answer
    could differ from mongod's. It now spills the digest's merged centroids
    and merges them back as digests, as mongod does.

    Linux x86-64 only: macOS arm64 mongod 8.2.11 contracts `Centroid::add`'s
    `mean + d * (w2 / w)` into a fused multiply-add, one ulp away
    (5551.627244897959 against 5551.62724489796, measured 2026-11-24; this
    server with `mul_add` gave macOS's answer exactly). Linux mongod is the
    exemplar build, and its x86-64 code has no FMA."""
    only_on("x86_64", why="macOS arm64 mongod fuses Centroid::add's multiply-add")
    assert _spilled_medians(rust_mongo)[0] == _spilled_medians(mongod)[0]
