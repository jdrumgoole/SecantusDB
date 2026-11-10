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

#: Between one filter test and two: mongod's write takes as long as the
#: `find` (1.0 s for both, measured), this server's took twice as long.
_BUDGET = 1.5
#: Paired rounds of a `find` and the write; the fastest of each are compared.
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


def _write_to_find_ratios(client: Any) -> dict[str, tuple[float, list, list]]:
    """For each write of one slow document: the fastest of its rounds over
    the fastest `find` with the same filter, timed in alternating pairs, and
    the rounds' times.

    The fastest, not one sample or the median: the filter is CPU work, which
    load only ever slows, so each side's fastest round is its own cost. A
    single `find`'s budget, the slower of two beside each write, and the
    median of three rounds each failed a write on a loaded CI runner that
    tested its filter once (#2608, #2622, #2634, 2026-11-10: `delete`'s
    median 1.66). A filter tested twice is twice the work in every round."""
    slow = Slow(client)
    out = {}
    for name, command in _WRITES.items():
        finds, writes = [], []
        for _ in range(_ROUNDS):
            slow.seed(1, 0.5)
            began = time.monotonic()
            slow.run({"find": "c", "filter": SLOW})
            finds.append(round(time.monotonic() - began, 3))
            slow.seed(1, 0.5)
            began = time.monotonic()
            reply = slow.run(command)
            writes.append(round(time.monotonic() - began, 3))
            assert reply.get("ok") == 1 and not reply.get("writeErrors"), (name, reply)
        out[name] = (round(min(writes) / min(finds), 3), finds, writes)
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
    assert all(r < _BUDGET for r, _, _ in theirs.values()), theirs
    ours = _write_to_find_ratios(rust_mongo)
    over = {name: v for name, v in ours.items() if v[0] >= _BUDGET}
    # Each over-budget write with its rounds' times (finds, writes): the
    # measurement itself, so a failure says whether it was load or work.
    assert not over, (
        f"ours over {_BUDGET}: {over}; mongod's ratios {[v[0] for v in theirs.values()]}"
    )
