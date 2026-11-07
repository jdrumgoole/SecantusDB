"""Divergences found while moving change stream pre-images into
`config.system.preimages` (RM-366, 2026-11-07), each measured against
mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest


def _delete_many_shape(client: Any, pre_images: bool) -> list:
    """Delete three documents with one `deleteMany`, and how the oplog logs
    it: each entry as its op, an `applyOps` one with its operations' count."""
    db = client.rm389
    db.drop_collection("d")
    options = {"changeStreamPreAndPostImages": {"enabled": True}} if pre_images else {}
    db.create_collection("d", **options)
    db.d.insert_many([{"_id": i} for i in range(3)])
    uuid = next(c for c in db.list_collections() if c["name"] == "d")["info"]["uuid"]
    last = next(client.local["oplog.rs"].find().sort("$natural", -1).limit(1))["ts"]
    db.d.delete_many({})
    out = []
    for e in client.local["oplog.rs"].find({"ts": {"$gt": last}}).sort("$natural", 1):
        ops = e.get("o", {}).get("applyOps") if e["op"] == "c" else None
        if ops is not None and all(op.get("ui") == uuid for op in ops):
            out.append(("applyOps", len(ops)))
        elif e.get("ui") == uuid:
            out.append((e["op"],))
    return out


@pytest.mark.backlog("RM-389")
def test_a_delete_many_over_pre_images_is_not_batched(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 writes a `deleteMany` of a collection that records
    pre-images as one `d` entry per document, where it batches the same
    delete of a plain collection into one `applyOps`. This server batches
    both."""
    assert _delete_many_shape(mongod_rs, False) == [("applyOps", 3)]  # the reference
    theirs = _delete_many_shape(mongod_rs, True)
    assert theirs == [("d",), ("d",), ("d",)], theirs  # the reference, as measured
    assert _delete_many_shape(rust_mongo, True) == theirs
