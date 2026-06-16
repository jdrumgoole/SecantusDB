"""Storage-level tests for user (multi-document) transactions.

These drive ``begin_user_transaction`` / ``use_user_transaction`` /
``commit_user_transaction`` / ``abort_user_transaction`` directly —
no wire protocol, no registry. The dispatch-level conformance tests
live in ``tests/test_transactions.py``.
"""

from __future__ import annotations

import pytest
from bson.int64 import Int64

from secantus.storage import Storage, WriteConflictError

DB = "txndb"
COLL = "stuff"
LSID = {"id": b"\x01" * 16}


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    yield s
    s.close()


def find_ids(storage, **kw):
    return [d["_id"] for d in storage.find_matching(DB, COLL, {}, **kw)]


def test_own_writes_visible_inside_invisible_outside(storage):
    h = storage.begin_user_transaction()
    with storage.use_user_transaction(h):
        storage.insert(DB, COLL, [{"_id": 1, "x": "in-txn"}])
        assert find_ids(storage) == [1]
    # Outside the transaction (thread session, fresh snapshot): nothing.
    assert find_ids(storage) == []
    storage.abort_user_transaction(h)


def test_commit_makes_writes_visible(storage):
    h = storage.begin_user_transaction()
    with storage.use_user_transaction(h):
        n, errors = storage.insert(DB, COLL, [{"_id": 1}, {"_id": 2}])
        assert (n, errors) == (2, [])
    storage.commit_user_transaction(h)
    assert find_ids(storage) == [1, 2]


def test_abort_rolls_back_everything(storage):
    storage.insert(DB, COLL, [{"_id": 1, "v": "original"}])
    h = storage.begin_user_transaction()
    with storage.use_user_transaction(h):
        storage.insert(DB, COLL, [{"_id": 2}])
        storage.update_matching(DB, COLL, {"_id": 1}, {"$set": {"v": "txn"}})
        storage.delete_matching(DB, COLL, {"_id": 1}, limit=0)
        assert find_ids(storage) == [2]
    storage.abort_user_transaction(h)
    docs = storage.find_matching(DB, COLL, {})
    assert [d["_id"] for d in docs] == [1]
    assert docs[0]["v"] == "original"


def test_snapshot_pins_at_first_statement(storage):
    storage.insert(DB, COLL, [{"_id": "before"}])
    h = storage.begin_user_transaction()
    with storage.use_user_transaction(h):
        assert find_ids(storage) == ["before"]
    # Committed outside after the transaction's first statement…
    storage.insert(DB, COLL, [{"_id": "after"}])
    with storage.use_user_transaction(h):
        # …stays invisible to the pinned snapshot.
        assert find_ids(storage) == ["before"]
    storage.abort_user_transaction(h)
    # find() with no sort is insertion order now: "before" was inserted first.
    assert find_ids(storage) == ["before", "after"]


def test_two_transactions_conflict_is_write_conflict(storage):
    # Pre-create the collection: otherwise both transactions race to
    # write the same collection-registry row and the conflict (still a
    # correct WriteConflict, as in mongod) fires there instead of at
    # the document write this test pins.
    storage.insert(DB, COLL, [{"_id": 0}])
    ha = storage.begin_user_transaction()
    hb = storage.begin_user_transaction()
    with storage.use_user_transaction(ha):
        storage.insert(DB, COLL, [{"_id": 9, "by": "a"}])
    with storage.use_user_transaction(hb), pytest.raises(WriteConflictError):
        storage.insert(DB, COLL, [{"_id": 9, "by": "b"}])
    storage.abort_user_transaction(hb)
    storage.commit_user_transaction(ha)
    docs = storage.find_matching(DB, COLL, {"_id": 9})
    assert docs[0]["by"] == "a"


def test_oplog_buffered_until_commit_with_shared_stamps(storage):
    def oplog_rows():
        return storage.read_oplog(start_seq=0, limit=1000)

    baseline = len(oplog_rows())
    h = storage.begin_user_transaction()
    with storage.use_user_transaction(h):
        storage.insert(DB, COLL, [{"_id": 1}])
    with storage.use_user_transaction(h):
        storage.insert(DB, COLL, [{"_id": 2}])
        storage.update_matching(DB, COLL, {"_id": 1}, {"$set": {"u": True}})
    # Nothing emitted while in progress.
    assert len(oplog_rows()) == baseline
    last_seq = storage.commit_user_transaction(h, lsid_doc=LSID, txn_number=7)
    rows = oplog_rows()[baseline:]
    assert last_seq == rows[-1][0]
    assert [e["op"] for _, e in rows] == ["i", "i", "u"]
    # One shared commit timestamp + session/txn stamps on every entry.
    timestamps = {e["ts"] for _, e in rows}
    assert len(timestamps) == 1
    for _, entry in rows:
        assert entry["lsid"]["id"] == LSID["id"]
        assert entry["txnNumber"] == Int64(7)


def test_oplog_discarded_on_abort(storage):
    baseline = len(storage.read_oplog(start_seq=0, limit=1000))
    h = storage.begin_user_transaction()
    with storage.use_user_transaction(h):
        storage.insert(DB, COLL, [{"_id": 1}, {"_id": 2}])
    storage.abort_user_transaction(h)
    assert len(storage.read_oplog(start_seq=0, limit=1000)) == baseline


def test_non_txn_writes_unaffected_by_txn_machinery(storage):
    # The plain write path still emits oplog rows normally even while
    # another (idle) user transaction exists.
    h = storage.begin_user_transaction()
    baseline = len(storage.read_oplog(start_seq=0, limit=1000))
    storage.insert(DB, COLL, [{"_id": "plain"}])
    assert len(storage.read_oplog(start_seq=0, limit=1000)) == baseline + 1
    storage.abort_user_transaction(h)


def test_commit_with_no_statements_is_noop(storage):
    h = storage.begin_user_transaction()
    assert storage.commit_user_transaction(h) == 0
    # Idempotent / safe after close.
    storage.abort_user_transaction(h)


def test_abort_is_idempotent(storage):
    h = storage.begin_user_transaction()
    with storage.use_user_transaction(h):
        storage.insert(DB, COLL, [{"_id": 1}])
    storage.abort_user_transaction(h)
    storage.abort_user_transaction(h)
    assert find_ids(storage) == []


def test_close_rolls_back_open_transactions(tmp_path):
    s = Storage(str(tmp_path))
    h = s.begin_user_transaction()
    with s.use_user_transaction(h):
        s.insert(DB, COLL, [{"_id": "uncommitted"}])
    s.close()  # sweeps _all_sessions; closing the session rolls back
    reopened = Storage(str(tmp_path))
    try:
        assert reopened.find_matching(DB, COLL, {}) == []
    finally:
        reopened.close()
