"""``listIndexes`` can be paginated.

The cursor was registered under a ``db.$cmd.listIndexes.<coll>`` pseudo-namespace,
but drivers put the plain collection name in the follow-up ``getMore``'s
``collection`` field. The getMore ownership check -- which compares the caller's
claimed namespace against the cursor's stored one, and is right to exist --
therefore rejected every continuation with ``CursorNotFound`` (43). The practical
effect: any collection with more indexes than the batch size could not have its
index list read to the end.

Probed on mongod 8.3.4, and the answer differs per command, so this is not a
blanket "drop the $cmd prefix":

    listIndexes            ns = db.coll                  <- was wrong here
    listCollections        ns = db.$cmd.listCollections  <- already correct
    aggregate: 1 (no coll) ns = db.$cmd.aggregate        <- already correct
"""

from __future__ import annotations

import pymongo
import pytest

from secantus import SecantusDBServer


@pytest.fixture
def client(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    cli = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}", directConnection=True)
    try:
        yield cli
    finally:
        cli.close()
        srv.stop()


def _all_index_names(db, batch_size):
    reply = db.command({"listIndexes": "c", "cursor": {"batchSize": batch_size}})
    cur = reply["cursor"]
    names = [spec["name"] for spec in cur["firstBatch"]]
    cursor_id = cur["id"]
    guard = 0
    while cursor_id and guard < 20:
        more = db.command({"getMore": cursor_id, "collection": "c", "batchSize": batch_size})
        cursor_id = more["cursor"]["id"]
        names += [spec["name"] for spec in more["cursor"]["nextBatch"]]
        guard += 1
    return names


def _seed(db, n_indexes):
    db.c.drop()
    db.c.insert_one({"x": 1})
    for i in range(n_indexes):
        db.c.create_index(f"f{i}")


def test_cursor_ns_is_the_plain_collection(client) -> None:
    db = client["li_ns"]
    _seed(db, 2)
    reply = db.command({"listIndexes": "c", "cursor": {"batchSize": 2}})
    assert reply["cursor"]["ns"] == "li_ns.c"
    assert "$cmd" not in reply["cursor"]["ns"]


@pytest.mark.parametrize("batch_size", [1, 2, 3])
def test_paginated_list_indexes_returns_every_index(client, batch_size) -> None:
    """The regression: a second batch used to be unreachable entirely."""
    db = client[f"li_page{batch_size}"]
    _seed(db, 4)  # _id_ + f0..f3 == 5 indexes
    names = _all_index_names(db, batch_size)
    assert sorted(names) == sorted(["_id_", "f0_1", "f1_1", "f2_1", "f3_1"]), names


def test_getmore_on_a_foreign_collection_is_still_rejected(client) -> None:
    """The ownership check must survive the fix -- a getMore claiming a
    different collection must not be able to pull another cursor's pages."""
    db = client["li_sec"]
    _seed(db, 4)
    db.other.insert_one({"y": 1})
    reply = db.command({"listIndexes": "c", "cursor": {"batchSize": 2}})
    cursor_id = reply["cursor"]["id"]
    assert cursor_id != 0
    with pytest.raises(pymongo.errors.OperationFailure) as exc:
        db.command({"getMore": cursor_id, "collection": "other", "batchSize": 2})
    assert exc.value.code == 43


def test_list_collections_namespace_is_unchanged(client) -> None:
    """listCollections really IS `$cmd.listCollections` on mongod -- this fix
    must not turn into a blanket removal of the prefix."""
    db = client["li_lc"]
    for i in range(3):
        db[f"c{i}"].insert_one({"x": 1})
    reply = db.command({"listCollections": 1, "cursor": {"batchSize": 2}})
    assert reply["cursor"]["ns"] == "li_lc.$cmd.listCollections"
    more = db.command(
        {"getMore": reply["cursor"]["id"], "collection": "$cmd.listCollections", "batchSize": 2}
    )
    assert more["cursor"]["nextBatch"] is not None


def test_pymongo_helper_sees_all_indexes(client) -> None:
    """End-to-end through the driver's own helper, which drives the getMore."""
    db = client["li_drv"]
    _seed(db, 4)
    names = sorted(spec["name"] for spec in db.c.list_indexes())
    assert names == sorted(["_id_", "f0_1", "f1_1", "f2_1", "f3_1"])
