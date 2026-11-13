"""A divergence found while making a batched deleteMany test a document
again after a yield (RM-501, 2026-11-13), measured against mongod 8.2.11:
a delete's profile entry counts no yields.

The reference is mongod, asked live (`mongod_rs`, a one-member replica set);
the Rust server is `secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.test_interrupts import DB, SLOW, Slow


def _delete_many_yields(client: Any) -> Any:
    """The profile entry's `numYield` for a deleteMany of three documents
    that each take half a second to test."""
    slow = Slow(client)
    slow.seed(3, 0.5)
    slow.run({"profile": 2})
    try:
        slow.run({"delete": "c", "deletes": [{"q": SLOW, "limit": 0}]})
    finally:
        slow.run({"profile": 0})
    entries = slow.run({"find": "system.profile", "filter": {"op": "remove", "ns": f"{DB}.c"}})
    slow.run({"drop": "system.profile"})
    found = entries["cursor"]["firstBatch"]
    return found[-1].get("numYield") if found else None


@pytest.mark.backlog("RM-609")
def test_a_delete_many_profile_entry_counts_its_yields(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod's executor yields once its yield period (10 ms) has passed,
    before a `work()` call: a deleteMany of three slow documents yields
    before staging the second and third, before the call that finds the
    end, and between the documents of the batch it deletes, which it
    splits once the batch has run `batchedDeletesTargetBatchTimeMS` (5 ms):
    `numYield: 5`. This server's profile entry for a delete says 0."""
    theirs = _delete_many_yields(mongod_rs)
    assert theirs == 5, theirs
    assert _delete_many_yields(rust_mongo) == theirs
