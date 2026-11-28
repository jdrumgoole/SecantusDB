"""A gap found diagnosing a mongod_live failure on PR #3139 (2026-11-28):
mongod 8.2.11 sizes a batched delete's batches by the server parameters
`batchedDeletesTargetBatchDocs` (10) and `batchedDeletesTargetBatchTimeMS`
(5), which `getParameter` and `setParameter` take, and writes each batch it
commits as one oplog entry: an `applyOps` of its deletes, or, for a batch
of one, a plain `d` entry. This server knows neither parameter.

The reference is mongod, asked live (`mongod_rs`, a single-node replica
set, for its oplog); the Rust server is `secantusd-mdb` (`rust_mongo`).
"""

from __future__ import annotations

from typing import Any

import pytest

DB = "rm_found_batched_delete_params"


def _oplog_of_a_delete_in_batches_of_three(client: Any) -> dict:
    db = client[DB]
    db.drop_collection("c")
    db.c.insert_many([{"_id": i, "a": i} for i in range(10)])
    out: dict = {
        "defaults": {
            k: client.admin.command({"getParameter": 1, k: 1})[k]
            for k in ("batchedDeletesTargetBatchDocs", "batchedDeletesTargetBatchTimeMS")
        }
    }
    since = next(client["local"]["oplog.rs"].find().sort("$natural", -1).limit(1))["ts"]
    client.admin.command({"setParameter": 1, "batchedDeletesTargetBatchDocs": 3})
    try:
        out["n"] = db.command({"delete": "c", "deletes": [{"q": {"a": {"$gte": 6}}, "limit": 0}]})[
            "n"
        ]
    finally:
        client.admin.command({"setParameter": 1, "batchedDeletesTargetBatchDocs": 10})
        client.drop_database(DB)
    entries = client["local"]["oplog.rs"].find(
        {
            "ts": {"$gt": since},
            "$or": [{"op": "d", "ns": f"{DB}.c"}, {"op": "c", "o.applyOps.ns": f"{DB}.c"}],
        }
    )
    out["entries"] = [
        ["d", e["o"]["_id"]]
        if e["op"] == "d"
        else ["applyOps", [o["o"]["_id"] for o in e["o"]["applyOps"] if o["op"] == "d"]]
        for e in entries.sort("$natural", 1)
    ]
    return out


@pytest.mark.backlog("RM-848")
def test_a_batched_delete_commits_batches_of_its_target_size(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 (measured 2026-11-28): with
    `batchedDeletesTargetBatchDocs` set to 3, a deleteMany of `_id` 6 to 9
    writes `applyOps` of 8, 7, 6 and then a plain `d` of 9. This server
    answers `getParameter` of either parameter with InvalidOptions."""
    theirs = _oplog_of_a_delete_in_batches_of_three(mongod_rs)
    assert theirs["entries"] == [["applyOps", [8, 7, 6]], ["d", 9]], theirs
    assert _oplog_of_a_delete_in_batches_of_three(rust_mongo) == theirs
