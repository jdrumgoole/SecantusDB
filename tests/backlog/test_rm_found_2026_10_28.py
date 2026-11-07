"""Gap and regression tests for the divergences found on 2026-10-28 while
comparing a fresh server's `config` database with mongod's (RM-366)."""

from __future__ import annotations

import time
from typing import Any

import pytest
from bson import json_util

from tests.backlog.conftest import mongo_answer

# What a one-member mongod 8.2.11 set has in `config` within half a second
# of stepping up, every time (three fresh starts, 2026-10-28), beside
# `transactions`, `image_collection` and `system.preimages`.
STEP_UP_COLLECTIONS = [
    "analyzeShardKeySplitPoints",
    "sampledQueries",
    "sampledQueriesDiff",
    "system.indexBuilds",
]


def _canonical(value: Any) -> str:
    return json_util.dumps(value, json_options=json_util.CANONICAL_JSON_OPTIONS)


def _step_up_collections(client: Any) -> list:
    """Each of `STEP_UP_COLLECTIONS` as `listCollections` and `listIndexes`
    describe it (less the collection's UUID), once the server has had the
    half second mongod takes to create them."""
    deadline = time.monotonic() + 5
    while True:
        listed = {c["name"]: c for c in client.config.list_collections()}
        if all(n in listed for n in STEP_UP_COLLECTIONS) or time.monotonic() > deadline:
            break
        time.sleep(0.1)
    out = []
    for name in STEP_UP_COLLECTIONS:
        entry = listed.get(name)
        if entry is None:
            out.append([name, None])
            continue
        entry.get("info", {}).pop("uuid", None)
        indexes = list(client.config[name].list_indexes())
        out.append([name, _canonical(entry), _canonical(indexes)])
    return out


@pytest.mark.backlog_fixed("RM-366")
def test_a_set_member_creates_its_step_up_collections(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 creates four more `config` collections as a replica-set
    member steps up: the index builds it would resume, and the query
    sampler's three, each with an `expireAt` TTL index. This server created
    only `transactions` and `image_collection`."""
    theirs = _step_up_collections(mongod_rs)
    assert all(entry[1] is not None for entry in theirs), theirs  # the reference
    assert _step_up_collections(rust_mongo) == theirs


def _pre_image_record(client: Any) -> Any:
    """Update a document of a collection that records pre-images, and read
    back the `config.system.preimages` entry the update wrote: its fields,
    the `_id`'s fields, and the pre-image itself."""
    db = client.rm366pre
    db.drop_collection("p")
    db.create_collection("p", changeStreamPreAndPostImages={"enabled": True})
    db.p.insert_one({"_id": 1, "x": 1})
    db.p.update_one({"_id": 1}, {"$set": {"x": 2}})
    uuid = next(c for c in db.list_collections() if c["name"] == "p")["info"]["uuid"]

    def read() -> list:
        found = list(client.config["system.preimages"].find({"_id.nsUUID": uuid}))
        return [
            (sorted(d), sorted(d["_id"]), d["_id"]["applyOpsIndex"], d["preImage"]) for d in found
        ]

    return mongo_answer(read)


@pytest.mark.backlog_fixed("RM-366")
def test_pre_images_are_kept_in_config_system_preimages(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 keeps a change stream pre-image as a document of the
    clustered `config.system.preimages`: `{_id: {nsUUID, ts,
    applyOpsIndex}, operationTime, preImage}`, which a user can read. This
    server kept pre-images in a table of its own, and had no such
    collection."""
    theirs = _pre_image_record(mongod_rs)
    assert theirs == (
        "ok",
        [
            (
                ["_id", "operationTime", "preImage"],
                ["applyOpsIndex", "nsUUID", "ts"],
                0,
                {"_id": 1, "x": 1},
            )
        ],
    ), theirs  # the reference, as measured
    assert _pre_image_record(rust_mongo) == theirs


def _pre_images_catalog(client: Any) -> list:
    """`config.system.preimages` as `listCollections` (less its UUID) and
    `listIndexes` describe it."""
    entry = next(
        (c for c in client.config.list_collections() if c["name"] == "system.preimages"), None
    )
    if entry is None:
        return [None]
    entry.get("info", {}).pop("uuid", None)
    indexes = list(client.config["system.preimages"].list_indexes())
    return [_canonical(entry), _canonical(indexes)]


@pytest.mark.backlog_fixed("RM-366")
def test_config_system_preimages_is_listed_as_mongod_lists_it(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 creates `config.system.preimages` as a member steps up,
    clustered in the legacy form (`clusteredIndex: true`), with one
    clustered `_id_` index. This server had no such collection."""
    theirs = _pre_images_catalog(mongod_rs)
    assert theirs[0] is not None, theirs  # the reference
    assert _pre_images_catalog(rust_mongo) == theirs


def _transaction_pre_images(client: Any) -> Any:
    """Update two documents of a collection that records pre-images in one
    transaction, and read back the two pre-images: their places in the
    transaction's `applyOps` entry, the documents, and whether each one's
    `_id.ts` is that entry's time in `local.oplog.rs`."""
    db = client.rm366txn
    db.drop_collection("t")
    db.create_collection("t", changeStreamPreAndPostImages={"enabled": True})
    db.t.insert_many([{"_id": 1, "x": 1}, {"_id": 2, "x": 2}])
    with client.start_session() as s, s.start_transaction():
        db.t.update_one({"_id": 1}, {"$set": {"x": 10}}, session=s)
        db.t.update_one({"_id": 2}, {"$set": {"x": 20}}, session=s)
    uuid = next(c for c in db.list_collections() if c["name"] == "t")["info"]["uuid"]
    entry = next(
        client.local["oplog.rs"]
        .find({"o.applyOps.ui": uuid, "lsid": {"$exists": True}})
        .sort("$natural", -1)
        .limit(1)
    )
    found = client.config["system.preimages"].find({"_id.nsUUID": uuid})
    return sorted(
        (d["_id"]["applyOpsIndex"], d["preImage"], d["_id"]["ts"] == entry["ts"]) for d in found
    )


@pytest.mark.backlog_fixed("RM-366")
def test_a_transaction_s_pre_images_share_its_applyops_entry_s_time(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 files the pre-images of a transaction's writes under
    the time of the one `applyOps` entry it logs, numbering each write's
    place in it (`applyOpsIndex`)."""
    theirs = _transaction_pre_images(mongod_rs)
    assert theirs == [
        (0, {"_id": 1, "x": 1}, True),
        (1, {"_id": 2, "x": 2}, True),
    ], theirs  # the reference, as measured
    assert _transaction_pre_images(rust_mongo) == theirs
