"""A divergence found while making a write test its filter once per document
(RM-391, 2026-11-10), measured against mongod 8.2.11: mongod's batched
deleteMany tests each document twice.

The reference is mongod, asked live (`mongod_rs`, a one-member replica set;
`mongod_rs_no_yield`, the same with time-based yields off);
the Rust server is `secantusd-mdb` (`rust_mongo`).
"""

from __future__ import annotations

import time
from typing import Any

import pytest

from tests.backlog.opdebug import no_timed_yields
from tests.mongod_live.test_interrupts import SLOW, Slow

#: `serverStatus.batchedDeletes`'s counters that are not times.
_COUNTS = ("batches", "docs", "stagedSizeBytes", "refetchesDueToYield")


def _batched_deletes(slow: Slow) -> dict:
    return slow.run({"serverStatus": 1}, db="admin")["batchedDeletes"]


def _a_slow_document_deleted(client: Any) -> tuple[dict, str]:
    """What one deleteMany of one slow document (two seconds to test) added
    to `serverStatus.batchedDeletes`, and the timings to print beside it:
    the counts, which no load moves, and the statement's seconds."""
    slow = Slow(client)
    slow.seed(1, 2.0)
    before = _batched_deletes(slow)
    began = time.monotonic()
    reply = slow.run({"delete": "c", "deletes": [{"q": SLOW, "limit": 0}]})
    took = time.monotonic() - began
    after = _batched_deletes(slow)
    assert reply.get("n") == 1, reply
    delta = {k: after[k] - before[k] for k in after}
    return (
        {k: delta[k] for k in _COUNTS},
        f"{delta} in {took:.2f} s",
    )


@pytest.mark.backlog_fixed("RM-501")
def test_a_batched_delete_many_tests_each_document_twice(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod's batched multi-delete (`batchUserMultiDeletes`, on by
    default) stages a document in one `work()` call and deletes the batch
    in a later one. A document's test that took longer than the yield
    period (`internalQueryExecYieldPeriodMS`, 10 ms) is followed by a
    yield, which abandons the snapshot, so that `ensureStillMatches`
    (`write_stage_common.cpp`) reads the document again and tests it again,
    and counts it in `serverStatus.batchedDeletes.refetchesDueToYield`
    (`batched_delete_stage.cpp`, r8.2.11): a deleteMany of one slow
    document takes twice a `find` of it.

    The comparison is that counter, which no load moves (RM-803: a
    `maxTimeMS` of 1.5 times a `find` fitted on either server under load).
    A server that tested the document once counts none."""
    theirs, their_note = _a_slow_document_deleted(mongod_rs)
    assert theirs["refetchesDueToYield"] == 1, their_note
    ours, our_note = _a_slow_document_deleted(rust_mongo)
    assert ours == theirs, (our_note, their_note)


@pytest.mark.backlog_fixed("RM-501")
def test_a_batched_delete_many_without_a_yield_tests_each_document_once(
    rust_mongo: Any, mongod_rs_no_yield: Any
) -> None:
    """The retest follows a yield and only a yield: with time-based yields
    off, the same deleteMany tests the document once and counts no refetch,
    on mongod and on a server that never retests alike."""
    with no_timed_yields(rust_mongo):
        theirs, their_note = _a_slow_document_deleted(mongod_rs_no_yield)
        assert theirs["refetchesDueToYield"] == 0, their_note
        ours, our_note = _a_slow_document_deleted(rust_mongo)
        assert ours == theirs, (our_note, their_note)
