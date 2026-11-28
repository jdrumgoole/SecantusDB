"""RM-216: the fields of the operation rows `currentOp` lists.

mongod 8.2.11, measured 2026-11-28 (macOS arm64; the field names are no
platform's): the row of a running `find` that carries a session has
`currentOpTime`, `lsid`, `planSummary`, `queryFramework`, `numYields`,
`locks` and `waitingForLock`, and an idle cursor's `cursor` block has
`createdDate`, `lastAccessDate`, `nDocsReturned`, `nBatchesReturned` and
`originatingCommand`, and the row itself `lsid` and `planSummary`. This
server (0.5.3-beta.199) lists none of them. Only the names are compared,
never a value.
"""

from __future__ import annotations

import threading
import time
import uuid
from typing import Any

import bson
import pytest

from tests.mongod_live.test_interrupts import SLOW, Slow

RUNNING = {
    "currentOpTime",
    "lsid",
    "planSummary",
    "queryFramework",
    "numYields",
    "locks",
    "waitingForLock",
}
IDLE_ROW = {"lsid", "planSummary"}
IDLE_CURSOR = {"createdDate", "lastAccessDate", "nDocsReturned", "nBatchesReturned"} | {
    "originatingCommand"
}


def _running_row(client: Any) -> set:
    slow = Slow(client)
    slow.seed(1)
    lsid = {"id": bson.Binary(uuid.uuid4().bytes, 4)}
    thread = threading.Thread(target=lambda: slow.run({"find": "c", "filter": SLOW, "lsid": lsid}))
    thread.start()
    try:
        time.sleep(0.5)
        rows = slow.run(
            {
                "aggregate": 1,
                "pipeline": [{"$currentOp": {}}, {"$match": {"ns": "live_interrupts.c"}}],
                "cursor": {},
            },
            db="admin",
        )["cursor"]["firstBatch"]
    finally:
        thread.join()
    assert len(rows) == 1, rows
    return set(rows[0])


def _idle_row(client: Any) -> tuple[set, set]:
    db = client["rm216"]
    db.drop_collection("c")
    db.c.insert_many([{"_id": i} for i in range(5)])
    cursor = db.c.find().batch_size(2)
    next(cursor)
    try:
        rows = list(
            client.admin.aggregate(
                [
                    {"$currentOp": {"idleCursors": True}},
                    {"$match": {"type": "idleCursor", "ns": "rm216.c"}},
                ]
            )
        )
    finally:
        cursor.close()
    assert len(rows) == 1, rows
    return set(rows[0]), set(rows[0]["cursor"])


@pytest.mark.backlog("RM-216")
def test_a_running_operations_row_has_mongods_fields(rust_mongo: Any, mongod: Any) -> None:
    assert _running_row(mongod) >= RUNNING
    assert _running_row(rust_mongo) >= RUNNING


@pytest.mark.backlog("RM-216")
def test_an_idle_cursors_row_has_mongods_fields(rust_mongo: Any, mongod: Any) -> None:
    row, cursor = _idle_row(mongod)
    assert row >= IDLE_ROW and cursor >= IDLE_CURSOR, (row, cursor)
    row, cursor = _idle_row(rust_mongo)
    assert row >= IDLE_ROW and cursor >= IDLE_CURSOR, (row, cursor)
