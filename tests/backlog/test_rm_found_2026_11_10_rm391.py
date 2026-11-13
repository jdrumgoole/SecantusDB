"""A divergence found while making a write test its filter once per document
(RM-391, 2026-11-10), measured against mongod 8.2.11: mongod's batched
deleteMany tests each document twice.

The reference is mongod, asked live (`mongod_rs`, a one-member replica set);
the Rust server is `secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import time
from typing import Any

import pytest

from tests.mongod_live.test_interrupts import SLOW, Slow

#: A budget between one filter test and two.
_BUDGET = 1.5


def _delete_many_in_a_find_s_time(client: Any) -> Any:
    """A deleteMany of one slow document, given `_BUDGET` times what a
    `find` with the same filter takes on this server: its error code, or
    None."""
    slow = Slow(client)
    slow.seed(1, 1.0)
    began = time.monotonic()
    slow.run({"find": "c", "filter": SLOW})
    budget = max(1, int((time.monotonic() - began) * 1000 * _BUDGET))
    slow.seed(1, 1.0)
    reply = slow.run({"delete": "c", "deletes": [{"q": SLOW, "limit": 0}], "maxTimeMS": budget})
    return reply.get("code") or next((e["code"] for e in reply.get("writeErrors", [])), None)


@pytest.mark.backlog_fixed("RM-501")
def test_a_batched_delete_many_tests_each_document_twice(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod's batched multi-delete (`batchUserMultiDeletes`, on by
    default) tests a document's filter when it stages it and again when it
    deletes the batch, the statement having yielded in between, so a
    deleteMany of one slow document takes twice a `find` of it and runs out
    of a `maxTimeMS` half again as long (code 50). This server tested it
    once, and the delete fitted."""
    theirs = _delete_many_in_a_find_s_time(mongod_rs)
    assert theirs == 50, theirs
    assert _delete_many_in_a_find_s_time(rust_mongo) == theirs
