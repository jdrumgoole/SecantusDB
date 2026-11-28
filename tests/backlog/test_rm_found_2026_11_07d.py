"""A divergence found while listing a running `bulkWrite`'s op in
`$currentOp` (RM-324, 2026-11-07), measured against mongod 8.2.1: a write
tests its filter against each document twice.

The reference is mongod, asked live (`mongod_rs`, a one-member replica set);
the Rust server is `secantusd-mdb` (`rust_mongo`).

The count is made by the filter itself, never a clock (RM-814: a write's
`cpuNanos` over a `find`'s moved from 0.91 to 2.0 between rounds on CI, on
mongod's own side). A filter that draws `$rand` answers differently each
time it runs, so the share of a thousand documents a write matches counts
how many times it ran the filter: once gives half, twice gives a quarter.
"""

from __future__ import annotations

from typing import Any

import pytest

#: Documents (or statements) per write, and the filter each tests: true
#: when a fresh `$rand` draw is under one half.
_N = 1000
_COIN = {"$expr": {"$lt": [{"$rand": {}}, 0.5]}}
#: What a filter run once matches is half of `_N` (mongod 8.2.11 matched
#: 747, 720, 715, 702, 763 and 733 of 1,500 over its six kinds of write, and
#: 964 to 1,006 of 2,000); the standard deviation is 16, so the band is
#: eight of them either side.
#: A filter run twice matches a quarter, one that retries a miss three
#: quarters, and neither is near it.
_LOW, _HIGH = _N * 3 // 8, _N * 5 // 8


def _matched(client: Any) -> dict[str, int]:
    """How many of `_N` documents each write matched, over a filter that
    holds only when a `$rand` draw is under one half. A multi-document write
    reads `_N` documents once; a single-document one is `_N` statements, each
    over its own document, found by an unindexed `k` so that the write scans."""
    db = client["rm_391_tests_once"]
    out: dict[str, int] = {}

    def seed() -> None:
        db.drop_collection("c")
        db.c.insert_many([{"_id": i, "k": i, "n": 0} for i in range(_N)])

    def one(i: int) -> dict:
        return {"$and": [{"k": i}, _COIN]}

    try:
        seed()
        out["updateMany"] = db.command(
            "update", "c", updates=[{"q": _COIN, "u": {"$inc": {"n": 1}}, "multi": True}]
        )["n"]
        seed()
        out["deleteMany"] = db.command("delete", "c", deletes=[{"q": _COIN, "limit": 0}])["n"]
        seed()
        out["findAndModify"] = sum(
            db.command("findAndModify", "c", query=one(i), update={"$inc": {"n": 1}})["value"]
            is not None
            for i in range(_N)
        )
        seed()
        out["findAndModify remove"] = sum(
            db.command("findAndModify", "c", query=one(i), remove=True)["value"] is not None
            for i in range(_N)
        )
    finally:
        client.drop_database("rm_391_tests_once")
    return out


@pytest.mark.backlog_fixed("RM-391")
def test_a_write_tests_each_document_once(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod tests a write's filter against each document once, so a write
    over a coin-flip filter matches half of what it reads. This server tested
    it once in the read that picks the candidates and again in the storage
    write, a quarter, and ran out of such a `maxTimeMS` (code 50); the write
    now takes the rows the read matched, with the bytes it matched them as,
    and tests only a row that changed since. Counted, not timed (RM-814).
    A single `update` or `delete` that matches nothing tests again, which
    RM-855 holds."""
    theirs = _matched(mongod_rs)
    assert all(_LOW <= v <= _HIGH for v in theirs.values()), theirs
    ours = _matched(rust_mongo)
    assert all(_LOW <= v <= _HIGH for v in ours.values()), (ours, theirs)
