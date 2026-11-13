"""Gap tests found while working RM-390 (2026-11-13).

The reference is mongod 8.2.11, asked live (`mongod_rs`, a one-member
replica set); the Rust server is `secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.mongod_live.conftest import canonical


def _profiled_counts(client: Any) -> list:
    """The `keysExamined` and `docsExamined` of a profiled updateMany and
    find through index `a_1`, as canonical extended JSON (which keeps their
    BSON types)."""
    db = client["rm_found_rm390"]
    db.drop_collection("c")
    db.drop_collection("system.profile")
    db.c.insert_many([{"_id": i, "a": i} for i in range(50)])
    db.c.create_index("a")
    db.command("profile", 2)
    try:
        db.command(
            {
                "update": "c",
                "updates": [{"q": {"a": {"$gte": 0}}, "u": {"$inc": {"n": 1}}, "multi": True}],
            }
        )
        db.c.find_one({"a": {"$gte": 0}})
    finally:
        db.command("profile", 0)
    return [
        canonical({k: e.get(k) for k in ("op", "keysExamined", "docsExamined")})
        for e in db["system.profile"].find({"op": {"$in": ["update", "query"]}})
    ]


@pytest.mark.backlog("RM-601")
def test_a_profile_entry_counts_what_it_examined_as_int32(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 writes a profile entry's `keysExamined` and
    `docsExamined` as int32 (measured 2026-11-13, an updateMany and a find:
    `{"$numberInt": "50"}`); this server writes them as int64, so a reader
    of `system.profile` that checks the type, or compares the documents
    byte for byte, sees a different entry."""
    theirs = _profiled_counts(mongod_rs)
    assert len(theirs) == 2, theirs
    assert _profiled_counts(rust_mongo) == theirs
