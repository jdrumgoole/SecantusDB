"""RM-843: a write waits for a running write's whole statement.

mongod 8.2.11 (measured 2026-11-28 on macOS arm64; the item records
`mongo:8.2.11` Linux alike): a `deleteMany` whose one document takes about
two seconds to test and match (`n: 0`) lets an `update` of that document,
sent 0.7 s in, through at once (`nModified: 1`). The delete tests the
document again after its yield, finds `n` changed and deletes nothing
(`n: 0`). This server (0.5.3-beta.199) answers the update after the delete,
`nModified: 0`, and the delete deletes the document (`n: 1`).

The test counts outcomes, never a duration.
"""

from __future__ import annotations

import threading
import time
from typing import Any

import pytest

from tests.mongod_live.test_interrupts import SLOW_EXPR, Slow


def _outcome(client: Any) -> dict:
    slow = Slow(client)
    slow.seed(1)
    out: dict = {}

    def delete() -> None:
        out["delete"] = slow.run(
            {
                "delete": "c",
                "deletes": [{"q": {"$and": [{"$expr": SLOW_EXPR}, {"n": 0}]}, "limit": 0}],
            }
        )

    thread = threading.Thread(target=delete)
    thread.start()
    time.sleep(0.7)
    update = slow.run({"update": "c", "updates": [{"q": {"_id": 0}, "u": {"$set": {"n": 1}}}]})
    thread.join()
    return {
        "nModified": update["nModified"],
        "deleted": out["delete"]["n"],
        "left": slow.state(),
    }


@pytest.mark.backlog("RM-843")
def test_an_update_does_not_wait_for_a_running_delete_many(rust_mongo: Any, mongod_rs: Any) -> None:
    theirs = _outcome(mongod_rs)
    assert theirs == {"nModified": 1, "deleted": 0, "left": (1, 1)}, theirs
    assert _outcome(rust_mongo) == theirs
