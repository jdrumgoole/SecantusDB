"""A divergence found while listing a running `bulkWrite`'s op in
`$currentOp` (RM-324, 2026-11-07), measured against mongod 8.2.1: a write
tests its filter against each document twice.

The reference is mongod, asked live (`mongod_rs`, a one-member replica set);
the Rust server is `secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import json
import statistics
import time
from typing import Any

import pytest

from tests.backlog.opdebug import no_timed_yields
from tests.mongod_live.test_interrupts import DB, SLOW, Slow

#: Between one filter test and two: mongod's write takes as long as the
#: `find` (1.0 s for both, measured), this server's took twice as long.
_BUDGET = 1.5
#: Paired rounds of a `find` and the write; the median of the rounds'
#: write-over-find ratios is compared.
_ROUNDS = 5

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


def _timed(client: Any, is_mongod: bool, slow: Slow, command: dict) -> tuple[float | None, float]:
    """`command`'s CPU time from its `Slow query` line's `cpuNanos` (Linux;
    both servers print it there, and neither elsewhere), and its wall time."""
    name = next(iter(command))
    log = client.admin.command("getLog", "global")["log"]
    last = log[-1] if log else None
    client[DB].command("profile", 0, slowms=-1 if is_mongod else 0)
    try:
        began = time.monotonic()
        reply = slow.run(command)
        wall = time.monotonic() - began
    finally:
        client[DB].command("profile", 0, slowms=100)
    assert reply.get("ok") == 1 and not reply.get("writeErrors"), (name, reply)
    log = client.admin.command("getLog", "global")["log"]
    if last is not None and last in log:
        log = log[len(log) - log[::-1].index(last) :]
    cpu = None
    for raw in log:
        entry = json.loads(raw)
        attr = entry.get("attr", {})
        if (
            entry.get("msg") == "Slow query"
            and str(attr.get("ns", "")).startswith(DB + ".")
            and next(iter(attr.get("command", {})), "") == name
            and "cpuNanos" in attr
        ):
            cpu = attr["cpuNanos"] / 1e9
    return cpu, wall


def _write_to_find_ratios(client: Any, is_mongod: bool) -> dict[str, tuple]:
    """For each write of one slow document: its cost over a `find` with the
    same filter, the median of each round's write over the `find` run just
    before it, with the basis and the rounds' times.

    The ratio is taken within a round, never fastest write over fastest
    `find`: each round's two operations run back to back and see the same
    load, while the fastest `find` of all rounds can come from a moment the
    writes never had. That form failed on CI three times on mongod's own
    side (RM-547: 1.602, 1.593, then on PR #2703 2026-11-12, `cpu` rounds of
    one `find` ranging 0.37 to 0.6 s). The median of five lets two rounds go
    wrong either way; the budget stays 1.5, which separates a filter tested
    once from one tested twice.

    The cost is CPU time where the servers report it (`cpuNanos`, Linux), so
    a loaded runner cannot move it, and wall time elsewhere, the fastest round
    being the one load slowed least. Wall time alone failed on CI in three
    forms on 2026-11-10 (#2608, #2622, #2634), the last when mongod's own
    rounds of one operation ranged 0.58 to 1.59 s (#2638). Time-based yields
    are off on both servers: under load mongod yields mid-write and tests the
    document again when it resumes, which is the load's doing, not the
    write's. A filter tested twice is twice the CPU."""
    slow = Slow(client)
    out = {}
    with no_timed_yields(client):
        for name, command in _WRITES.items():
            finds, writes = [], []
            for _ in range(_ROUNDS):
                slow.seed(1, 0.5)
                finds.append(_timed(client, is_mongod, slow, {"find": "c", "filter": SLOW}))
                slow.seed(1, 0.5)
                writes.append(_timed(client, is_mongod, slow, command))
            cpu = all(c is not None for c, _ in finds + writes)
            basis = 0 if cpu else 1
            f = [round(t[basis], 3) for t in finds]
            w = [round(t[basis], 3) for t in writes]
            ratio = statistics.median(b / a for a, b in zip(f, w, strict=True))
            out[name] = (round(ratio, 3), "cpu" if cpu else "wall", f, w)
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
    theirs = _write_to_find_ratios(mongod_rs, True)
    assert all(v[0] < _BUDGET for v in theirs.values()), theirs
    ours = _write_to_find_ratios(rust_mongo, False)
    over = {name: v for name, v in ours.items() if v[0] >= _BUDGET}
    # Each over-budget write with its rounds' times (finds, writes): the
    # measurement itself, so a failure says whether it was load or work.
    assert not over, (
        f"ours over {_BUDGET}: {over}; mongod's ratios {[v[0] for v in theirs.values()]}"
    )
