"""A gap found while fixing RM-390's CI test (2026-11-13): a read whose plan
two indexes contest reads every match on this server, where mongod 8.2.11
stops once the read has what it needs.

The reference is mongod, asked live (`mongod_rs`, a one-member replica set);
the Rust server is `secantusd-rs` (`rust_mongo`). The test compares how each
server's time grows with the collection, not the times themselves, so a
debug build or a slow runner does not move it: a read that stops at its
limit costs the same over 10,000 documents and 100,000, one that reads
every match costs ten times as much.
"""

from __future__ import annotations

import time
from collections.abc import Callable
from typing import Any

import pytest

_QUERY = {"a": {"$gte": 0}, "b": {"$gte": 0}}
#: A tenfold collection: a read that stops at what it needs costs about the
#: same over both, one that reads every match about ten times as much.
#: Three separates the two with room for timing noise.
_RATIO = 3.0


def _seed(db: Any, n: int) -> None:
    db.drop_collection("c")
    for start in range(0, n, 10_000):
        db.c.insert_many(
            [{"_id": i, "a": i, "b": i % 7, "n": 0} for i in range(start, min(n, start + 10_000))]
        )
    db.c.create_index("a")
    db.c.create_index("b")


def _best_ms(call: Callable[[], Any]) -> float:
    best = float("inf")
    for _ in range(3):
        began = time.monotonic()
        call()
        best = min(best, time.monotonic() - began)
    return best * 1000


def _growth(client: Any) -> dict[str, bool]:
    """For each operation over the contested query: whether its time over
    100,000 documents is within `_RATIO` of its time over 10,000."""
    db = client["rm_found_contested_reads"]
    reads: dict[str, Callable[[], Any]] = {
        "find with a limit": lambda: list(db.c.find(_QUERY).limit(101)),
        "updateOne": lambda: db.command(
            "update", "c", updates=[{"q": _QUERY, "u": {"$inc": {"z": 1}}}]
        ),
        "explain at queryPlanner": lambda: db.command(
            "explain", {"find": "c", "filter": _QUERY, "limit": 101}, verbosity="queryPlanner"
        ),
    }
    times: dict[int, dict[str, float]] = {}
    try:
        for n in (10_000, 100_000):
            _seed(db, n)
            times[n] = {name: _best_ms(call) for name, call in reads.items()}
    finally:
        client.drop_database("rm_found_contested_reads")
    # A floor of 1 ms keeps a sub-millisecond read's jitter from counting
    # as growth.
    return {
        name: max(times[100_000][name], 1.0) <= _RATIO * max(times[10_000][name], 1.0)
        for name in reads
    }


@pytest.mark.backlog("RM-610")
def test_a_contested_read_costs_what_it_returns(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod's cost of each read is flat in the collection's size; this
    server's grows with it."""
    theirs = _growth(mongod_rs)
    assert all(theirs.values()), theirs
    assert _growth(rust_mongo) == theirs
