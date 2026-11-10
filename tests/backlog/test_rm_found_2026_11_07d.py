"""A divergence found while listing a running `bulkWrite`'s op in
`$currentOp` (RM-324, 2026-11-07), measured against mongod 8.2.1: a write
tests its filter against each document twice.

The reference is mongod, asked live (`mongod_rs`, a one-member replica set);
the Rust server is `secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import time
from typing import Any

import pytest

from tests.mongod_live.test_interrupts import SLOW, Slow

#: A budget between one filter test and two: mongod's write takes as long as
#: the `find` (1.0 s for both, measured), this server's twice as long.
_BUDGET = 1.5


def _writes_in_a_find_s_time(client: Any) -> list:
    """Each write of one slow document, given `_BUDGET` times what a `find`
    with the same filter takes on this server: its error code, or None."""
    slow = Slow(client)

    def budget() -> int:
        # Paired with the write it is for, and the slower of two `find`s: one
        # `find` timed before all five writes gave a budget the later writes
        # overran on a loaded CI runner, testing their filter once
        # (snapshot #2608, 2026-11-10; its rerun passed). A filter tested
        # twice still takes about twice a `find`, past the budget.
        took = []
        for _ in range(2):
            slow.seed(1, 1.0)
            began = time.monotonic()
            slow.run({"find": "c", "filter": SLOW})
            took.append(time.monotonic() - began)
        return max(1, int(max(took) * 1000 * _BUDGET))

    writes = {
        "update": {"update": "c", "updates": [{"q": SLOW, "u": {"$inc": {"n": 1}}}]},
        "updateMany": {
            "update": "c",
            "updates": [{"q": SLOW, "u": {"$inc": {"n": 1}}, "multi": True}],
        },
        "delete": {"delete": "c", "deletes": [{"q": SLOW, "limit": 1}]},
        "findAndModify": {"findAndModify": "c", "query": SLOW, "update": {"$inc": {"n": 1}}},
        "findAndModify remove": {"findAndModify": "c", "query": SLOW, "remove": True},
    }
    out = []
    for name, command in writes.items():
        ms = budget()
        slow.seed(1, 1.0)
        reply = slow.run({**command, "maxTimeMS": ms})
        code = reply.get("code") or next((e["code"] for e in reply.get("writeErrors", [])), None)
        out.append((name, code))
    return out


@pytest.mark.backlog_fixed("RM-391")
def test_a_write_tests_each_document_once(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod tests a write's filter against each document once, so an
    update or delete of one document takes as long as a `find` of it and
    fits in a `maxTimeMS` half again as long. This server tested it once in
    the read that picks the candidates and again in the storage write, and
    ran out of that budget (code 50); the write now takes the rows the read
    matched, with the bytes it matched them as, and tests only a row that
    changed since."""
    theirs = _writes_in_a_find_s_time(mongod_rs)
    assert theirs == [(name, None) for name, _ in theirs], theirs
    assert _writes_in_a_find_s_time(rust_mongo) == theirs
