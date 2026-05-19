"""Tests for per-write ``writeConcern: {j: true}`` routing.

The b20 ``sync_on_commit`` knob enables WT's per-commit fsync at
the connection level — every write on the daemon goes through
``transaction_sync=(enabled=true,method=fsync)``. This slice
finishes the story by routing the per-write ``writeConcern.j``
flag from the wire down to ``_batch_transaction`` so a single
write can opt into journal-durable semantics even when the daemon
is otherwise running with ``sync_on_commit=false``.

The hard part to assert is "did the commit actually fsync." We
can't observe WT's fsync syscall directly, but we can:

* Verify the routing — the journal flag reaches Storage and is
  threaded through ``_batch_transaction``.
* Verify correctness end-to-end — a ``j: true`` write succeeds
  and the doc is visible (so we didn't break the commit path).
* Verify the negative — ``j: false`` (or absent) still works.
* Verify ``insert`` / ``update`` / ``delete`` / ``findAndModify``
  all honour the flag.
"""

from __future__ import annotations

from unittest.mock import patch

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer
from secantus.storage import Storage

# ---------------------------------------------------------------------------
# Storage-layer: the kwarg reaches _batch_transaction(sync=...)
# ---------------------------------------------------------------------------


def test_storage_insert_threads_journal_to_batch_transaction(tmp_path) -> None:
    """``Storage.insert(journal=True)`` must reach ``_batch_transaction``
    with ``sync=True``. Patch the context manager and inspect."""
    storage = Storage(str(tmp_path / "wt"))
    try:
        with patch.object(storage, "_batch_transaction", wraps=storage._batch_transaction) as bt:
            storage.insert("d", "c", [{"_id": 1}], journal=True)
            assert bt.call_args.kwargs == {"sync": True}
        with patch.object(storage, "_batch_transaction", wraps=storage._batch_transaction) as bt:
            storage.insert("d", "c", [{"_id": 2}], journal=False)
            assert bt.call_args.kwargs == {"sync": False}
    finally:
        storage.close()


def test_storage_update_threads_journal(tmp_path) -> None:
    storage = Storage(str(tmp_path / "wt"))
    try:
        storage.insert("d", "c", [{"_id": 1, "v": 1}])
        with patch.object(storage, "_batch_transaction", wraps=storage._batch_transaction) as bt:
            storage.update_matching("d", "c", {"_id": 1}, {"$set": {"v": 2}}, journal=True)
            assert bt.call_args.kwargs == {"sync": True}
    finally:
        storage.close()


def test_storage_delete_threads_journal(tmp_path) -> None:
    storage = Storage(str(tmp_path / "wt"))
    try:
        storage.insert("d", "c", [{"_id": 1}])
        with patch.object(storage, "_batch_transaction", wraps=storage._batch_transaction) as bt:
            storage.delete_matching("d", "c", {"_id": 1}, journal=True)
            assert bt.call_args.kwargs == {"sync": True}
    finally:
        storage.close()


def test_storage_default_journal_is_false(tmp_path) -> None:
    """Existing call sites that don't pass ``journal`` keep the
    pre-slice behaviour (commit without forcing sync=on)."""
    storage = Storage(str(tmp_path / "wt"))
    try:
        with patch.object(storage, "_batch_transaction", wraps=storage._batch_transaction) as bt:
            storage.insert("d", "c", [{"_id": 1}])
            assert bt.call_args.kwargs == {"sync": False}
    finally:
        storage.close()


# ---------------------------------------------------------------------------
# Wire layer: writeConcern.j propagates through pymongo
# ---------------------------------------------------------------------------


@pytest.fixture
def server_and_client(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    srv.start()
    client = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
    try:
        yield srv, client
    finally:
        client.close()
        srv.stop()


def test_wire_insert_with_journal_true_succeeds(server_and_client) -> None:
    """End-to-end: insert with ``writeConcern: {w: 1, j: true}`` over
    pymongo lands, and the doc is readable. Per-commit fsync is a
    storage-level concern we can't observe over the wire — the
    correctness guarantee here is "the write path didn't break when
    the j:true field threaded through.\""""
    srv, client = server_and_client
    coll = client["d"].get_collection("c", write_concern=_wc(j=True))
    coll.insert_one({"_id": 1, "v": "j-true"})
    assert list(client["d"]["c"].find()) == [{"_id": 1, "v": "j-true"}]


def test_wire_update_with_journal_true_succeeds(server_and_client) -> None:
    srv, client = server_and_client
    client["d"]["c"].insert_one({"_id": 1, "v": 1})
    coll = client["d"].get_collection("c", write_concern=_wc(j=True))
    res = coll.update_one({"_id": 1}, {"$set": {"v": 2}})
    assert res.modified_count == 1
    assert client["d"]["c"].find_one({"_id": 1})["v"] == 2


def test_wire_delete_with_journal_true_succeeds(server_and_client) -> None:
    srv, client = server_and_client
    client["d"]["c"].insert_many([{"_id": 1}, {"_id": 2}])
    coll = client["d"].get_collection("c", write_concern=_wc(j=True))
    res = coll.delete_one({"_id": 1})
    assert res.deleted_count == 1
    assert list(client["d"]["c"].find()) == [{"_id": 2}]


def test_wire_find_and_modify_with_journal_true_succeeds(
    server_and_client,
) -> None:
    """findAndModify's update + remove + upsert paths all hit
    ``update_matching`` / ``delete_matching`` and should honour
    j:true the same way."""
    srv, client = server_and_client
    client["d"]["c"].insert_one({"_id": 1, "v": 1})
    coll = client["d"].get_collection("c", write_concern=_wc(j=True))

    # update path
    res = coll.find_one_and_update({"_id": 1}, {"$set": {"v": 2}}, return_document=False)
    assert res == {"_id": 1, "v": 1}
    assert client["d"]["c"].find_one({"_id": 1})["v"] == 2

    # upsert path
    res = coll.find_one_and_update(
        {"_id": 99}, {"$set": {"v": "new"}}, upsert=True, return_document=False
    )
    assert client["d"]["c"].find_one({"_id": 99})["v"] == "new"

    # remove path
    res = coll.find_one_and_delete({"_id": 1})
    assert res == {"_id": 1, "v": 2}


def test_wire_writeconcern_j_routes_through_to_storage(server_and_client) -> None:
    """The end-to-end happy path is great, but the value-add of this
    slice is that ``journal=True`` actually makes it down to
    ``_batch_transaction``. Patch and assert."""
    srv, client = server_and_client
    with patch.object(
        srv.storage,
        "_batch_transaction",
        wraps=srv.storage._batch_transaction,
    ) as bt:
        coll = client["d"].get_collection("c", write_concern=_wc(j=True))
        coll.insert_one({"_id": 1})
        # The first batch_transaction call from this insert was
        # invoked with sync=True. Other internal calls (e.g.
        # collection_options bootstrap) are not write-path; they
        # don't go through _batch_transaction.
        sync_calls = [c for c in bt.call_args_list if c.kwargs.get("sync") is True]
        assert sync_calls, "insert with j:true should have invoked _batch_transaction(sync=True)"


def test_wire_writeconcern_j_false_does_not_force_sync(server_and_client) -> None:
    """The negative case — without ``j: true`` the storage call must
    NOT force sync=on. Otherwise the per-write knob is dead code."""
    srv, client = server_and_client
    with patch.object(
        srv.storage,
        "_batch_transaction",
        wraps=srv.storage._batch_transaction,
    ) as bt:
        # No write concern.
        client["d"]["c"].insert_one({"_id": 1})
        # Explicit j:false.
        coll = client["d"].get_collection("c", write_concern=_wc(j=False))
        coll.insert_one({"_id": 2})
        sync_true_calls = [c for c in bt.call_args_list if c.kwargs.get("sync") is True]
        assert not sync_true_calls, (
            "writes without j:true should not have forced sync=on on _batch_transaction"
        )


# ---------------------------------------------------------------------------
# Helper
# ---------------------------------------------------------------------------


def _wc(*, j: bool):
    """pymongo WriteConcern factory imported lazily so the test file's
    top-of-file imports stay focused on test machinery."""
    from pymongo.write_concern import WriteConcern

    return WriteConcern(j=j)
