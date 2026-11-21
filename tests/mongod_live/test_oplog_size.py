"""The oplog's size and its first entry, against a live mongod 8.2
(storage-model 4b-5 and 4b-6).

`replSetResizeOplog` changes the oplog's `size`, which `collStats.maxSize` and
`listCollections` both report; `size` is in megabytes and the fraction is kept
(`1000.5` gives 1049100288). A new member's oplog begins with the
`initiating set` no-op, without a `t`, and its `local.replset.*` documents
have the shapes `tools/probes/mongod_replset_state.py` measured. The default
size is platform-bound (192 MB on a Mac, 5% of the free disk elsewhere), so it
is compared only where the two servers run on one platform: here, always.
"""

from __future__ import annotations

import sys
from typing import Any

from tests.backlog.conftest import mongo_answer
from tests.mongod_live.conftest import same


def _oplog_size(client: Any) -> tuple:
    stats = client.local.command("collStats", "oplog.rs")
    options = next(client.local.list_collections(filter={"name": "oplog.rs"}))["options"]
    return stats["maxSize"], options["size"], options["capped"], options["autoIndexId"]


def test_default_size_and_resize(rust_mongo: Any, mongod_rs: Any) -> None:
    ours, theirs = _oplog_size(rust_mongo), _oplog_size(mongod_rs)
    if sys.platform == "darwin":
        same(ours, theirs)  # mongod's macOS default, 192 MB, to the byte
    else:
        # 5% of the free disk of each one's own directory, at most 50 GB and
        # at least 990 MB: the two differ by what each filesystem had free.
        for size in (ours[0], theirs[0]):
            assert 990 << 20 <= size <= 50 << 30, size
        same(ours[2:], theirs[2:])
    for size in (1000, 1100, 1000.5):
        for client in (rust_mongo, mongod_rs):
            assert client.admin.command("replSetResizeOplog", size=size)["ok"] == 1.0
        same(_oplog_size(rust_mongo), _oplog_size(mongod_rs))
    for client in (rust_mongo, mongod_rs):
        assert client.admin.command("replSetResizeOplog", minRetentionHours=0.5)["ok"] == 1.0
    same(_oplog_size(rust_mongo), _oplog_size(mongod_rs))


def test_resize_errors(rust_mongo: Any, mongod_rs: Any) -> None:
    for cmd in (
        {"replSetResizeOplog": 1, "size": 989},
        {"replSetResizeOplog": 1, "minRetentionHours": -1},
        {"replSetResizeOplog": 1, "size": "x"},
        {"replSetResizeOplog": 1, "foo": 1},
    ):
        same(
            mongo_answer(lambda cmd=cmd: rust_mongo.admin.command(cmd)),
            mongo_answer(lambda cmd=cmd: mongod_rs.admin.command(cmd)),
        )


def test_the_oplog_begins_with_the_initiating_set(rust_mongo: Any, mongod_rs: Any) -> None:
    def first(client: Any) -> dict:
        entry = next(client.local["oplog.rs"].find().sort("$natural", 1).limit(1))
        # The time and the wall clock are the servers' own.
        return {k: v for k, v in entry.items() if k not in ("ts", "wall")} | {"fields": list(entry)}

    same(first(rust_mongo), first(mongod_rs))


def test_the_replica_set_state_documents(rust_mongo: Any, mongod_rs: Any) -> None:
    def shape(client: Any, db: str, coll: str) -> list:
        # Field names, order and BSON types; the values are each server's own.
        return [[(k, type(v).__name__) for k, v in doc.items()] for doc in client[db][coll].find()]

    for db, coll in (
        ("local", "replset.minvalid"),
        ("local", "replset.oplogTruncateAfterPoint"),
        ("local", "replset.election"),
        ("local", "replset.initialSyncId"),
        ("local", "system.rollback.id"),
    ):
        same(shape(rust_mongo, db, coll), shape(mongod_rs, db, coll))
