"""Gap test for what working RM-395 found, 2026-11-10: a sort by a key
most documents share spills runs whose documents that compare equal are
not stored as mongod 8.2.11 stores them.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import only_on
from tests.backlog.test_rm_found_2026_10_26 import _spill_db, _tied_order, knob


def _tied_sort_storage(client: Any) -> Any:
    db = _spill_db(client)
    with knob(client, "internalQueryMaxBlockingSortMemoryUsageBytes", 50_000):
        db.command({"profile": 2})
        try:
            list(db.c.find({}, sort=[("k", -1)], comment="tied"))
        finally:
            db.command({"profile": 0})
    entry = db.system.profile.find_one({"command.comment": "tied"}) or {}
    return (entry.get("sortSpills"), entry.get("sortSpilledDataStorageSize"))


@pytest.mark.backlog("RM-526")
def test_tied_sort_spill_storage_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 on arm64 stores 51260 bytes for the 13 spills of 300
    documents sorted by `{k: -1}` (seven values of `k`); this server stores
    51242: the same spills, records and bytes, so the documents that compare
    equal sit in each run in another order. On Linux the two agree since
    RM-402 (libstdc++'s `std::sort`)."""
    only_on("darwin", why="Linux mongod and this server agree since RM-402")
    assert _tied_sort_storage(rust_mongo) == _tied_sort_storage(mongod)


@pytest.mark.backlog("RM-526")
def test_macos_tied_sort_order(rust_mongo: Any, mongod: Any) -> None:
    """macOS mongod 8.2.11 returns the 40 documents over three values of `k`
    a `$sort` after another stage sorts in libc++'s `std::sort` order (39,
    3, 27, 9, ...); this server returns them in the order they came."""
    only_on("darwin", why="Linux mongod and this server agree since RM-402")
    assert _tied_order(rust_mongo) == _tied_order(mongod)
