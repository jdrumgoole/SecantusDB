"""A divergence found while listing a running `bulkWrite`'s op in
`$currentOp` (RM-324, 2026-11-07), measured against mongod 8.2.1: a write
tests its filter against each document twice.

The reference is mongod, asked live (`mongod_rs`, a one-member replica set);
the Rust server is `secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import statistics
import time
from typing import Any

import pytest

from tests.mongod_live.test_interrupts import SLOW, Slow

#: Between one filter test and two: mongod's write takes as long as the
#: `find` (1.0 s for both, measured), this server's took twice as long.
_BUDGET = 1.5
#: Paired rounds of a `find` and the write; the medians are compared.
_ROUNDS = 3

_WRITES = {
    "update": {"update": "c", "updates": [{"q": SLOW, "u": {"$inc": {"n": 1}}}]},
    "updateMany": {
        "update": "c",
        "updates": [{"q": SLOW, "u": {"$inc": {"n": 1}}, "multi": True}],
    },
    "delete": {"delete": "c", "deletes": [{"q": SLOW, "limit": 1}]},
    "findAndModify": {"findAndModify": "c", "query": SLOW, "update": {"$inc": {"n": 1}}},
    "findAndModify remove": {"findAndModify": "c", "query": SLOW, "remove": True},
}


def _write_to_find_ratios(client: Any) -> dict[str, float]:
    """For each write of one slow document, the median of its time over the
    median of a `find` with the same filter, timed in alternating pairs.

    Medians of paired rounds, not one sample against a budget: a single
    `find` timed before the writes, and then the slower of two timed beside
    each write, both gave budgets a write overran on a loaded CI runner
    while testing its filter once (#2608, #2622, 2026-11-10). A filter
    tested twice takes about twice a `find` in every round."""
    slow = Slow(client)
    out = {}
    for name, command in _WRITES.items():
        finds, writes = [], []
        for _ in range(_ROUNDS):
            slow.seed(1, 0.5)
            began = time.monotonic()
            slow.run({"find": "c", "filter": SLOW})
            finds.append(time.monotonic() - began)
            slow.seed(1, 0.5)
            began = time.monotonic()
            reply = slow.run(command)
            writes.append(time.monotonic() - began)
            assert reply.get("ok") == 1 and not reply.get("writeErrors"), (name, reply)
        out[name] = statistics.median(writes) / statistics.median(finds)
    return out


@pytest.mark.backlog_fixed("RM-391")
def test_a_write_tests_each_document_once(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod tests a write's filter against each document once, so an
    update or delete of one document takes as long as a `find` of it, under
    half again as long. This server tested it once in the read that picks
    the candidates and again in the storage write, twice as long, and ran
    out of such a `maxTimeMS` (code 50); the write now takes the rows the
    read matched, with the bytes it matched them as, and tests only a row
    that changed since."""
    theirs = _write_to_find_ratios(mongod_rs)
    assert all(r < _BUDGET for r in theirs.values()), theirs
    ours = _write_to_find_ratios(rust_mongo)
    assert all(r < _BUDGET for r in ours.values()), f"ours {ours}, mongod {theirs}"
