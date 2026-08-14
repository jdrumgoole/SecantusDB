"""pymongo-driven conformance tests for multi-document transactions.

These run the real driver transaction API (``session.start_transaction``
/ ``commit_transaction`` / ``abort_transaction``) against a live
SecantusDBServer — the conformance proof that the wire shapes, error
codes, and ``TransientTransactionError`` labels match what pymongo
expects from mongod. The state-machine unit tests live in
``tests/test_transaction_registry.py``; storage-level WT-transaction
tests in ``tests/test_storage_user_txn.py``.
"""

from __future__ import annotations

import pymongo
import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure
from pymongo.read_concern import ReadConcern

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer):
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield mc
    finally:
        mc.close()


@pytest.fixture
def coll(client: MongoClient):
    c = client["txndb"]["things"]
    # Pre-create: implicit collection creation inside a transaction is
    # supported, but most tests want a baseline outside the txn anyway.
    c.insert_one({"_id": "seed"})
    c.delete_one({"_id": "seed"})
    return c


def test_commit_makes_writes_visible(client, coll):
    with client.start_session() as s, s.start_transaction():
        coll.insert_one({"_id": 1, "x": "a"}, session=s)
        coll.insert_one({"_id": 2, "x": "b"}, session=s)
    assert coll.count_documents({}) == 2


def test_abort_rolls_back(client, coll):
    coll.insert_one({"_id": 1, "v": "original"})
    with client.start_session() as s:
        s.start_transaction()
        coll.insert_one({"_id": 2}, session=s)
        coll.update_one({"_id": 1}, {"$set": {"v": "txn"}}, session=s)
        coll.delete_one({"_id": 1}, session=s)
        s.abort_transaction()
    docs = list(coll.find())
    assert docs == [{"_id": 1, "v": "original"}]


def test_read_your_own_writes_and_isolation(client, coll):
    coll.insert_one({"_id": "before"})
    with client.start_session() as s:
        s.start_transaction()
        coll.insert_one({"_id": "mine"}, session=s)
        # Inside: both visible.
        inside = {d["_id"] for d in coll.find({}, session=s)}
        assert inside == {"before", "mine"}
        # Outside (no session): uncommitted write invisible.
        outside = {d["_id"] for d in coll.find()}
        assert outside == {"before"}
        s.abort_transaction()


def test_snapshot_pins_at_first_statement(client, coll):
    coll.insert_one({"_id": "before"})
    with client.start_session() as s:
        s.start_transaction()
        assert {d["_id"] for d in coll.find({}, session=s)} == {"before"}
        # Committed outside after the transaction's first read…
        coll.insert_one({"_id": "after"})
        # …is not visible to the pinned snapshot.
        assert {d["_id"] for d in coll.find({}, session=s)} == {"before"}
        s.abort_transaction()
    assert {d["_id"] for d in coll.find()} == {"before", "after"}


def test_count_in_transaction_is_263(client, coll):
    with client.start_session() as s:
        s.start_transaction()
        with pytest.raises(OperationFailure) as excinfo:
            client["txndb"].command({"count": "things"}, session=s)
        assert excinfo.value.code == 263
        # The failed statement aborted the transaction server-side; the
        # driver-side abort gets NoSuchTransaction and swallows it.
        s.abort_transaction()


def test_aggregate_out_in_transaction_is_263(client, coll):
    coll.insert_one({"_id": 1})
    with client.start_session() as s:
        s.start_transaction()
        with pytest.raises(OperationFailure) as excinfo:
            list(coll.aggregate([{"$match": {}}, {"$out": "elsewhere"}], session=s))
        assert excinfo.value.code == 263
        s.abort_transaction()


def test_write_conflict_has_code_and_label(server, client, coll):
    coll.insert_one({"_id": 1, "v": 0})
    other = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        with client.start_session() as s1, other.start_session() as s2:
            s1.start_transaction()
            s2.start_transaction()
            coll.update_one({"_id": 1}, {"$set": {"v": 1}}, session=s1)
            other_coll = other["txndb"]["things"]
            with pytest.raises(OperationFailure) as excinfo:
                other_coll.update_one({"_id": 1}, {"$set": {"v": 2}}, session=s2)
            assert excinfo.value.code == 112
            assert excinfo.value.has_error_label("TransientTransactionError")
            # The losing transaction was aborted server-side: its commit
            # reports NoSuchTransaction, also transient-labeled.
            with pytest.raises(OperationFailure) as commit_err:
                s2.commit_transaction()
            assert commit_err.value.code == 251
            assert commit_err.value.has_error_label("TransientTransactionError")
            s1.commit_transaction()
    finally:
        other.close()
    assert coll.find_one({"_id": 1})["v"] == 1


def test_commit_is_idempotent_on_retry(client, coll):
    # The driver spec allows calling commitTransaction multiple times
    # (retries after e.g. an UnknownTransactionCommitResult); pymongo
    # re-sends the command, and the server must answer ok:1 without
    # re-applying anything.
    with client.start_session() as s:
        s.start_transaction()
        coll.insert_one({"_id": 1}, session=s)
        s.commit_transaction()
        s.commit_transaction()
    assert coll.count_documents({}) == 1


def test_duplicate_key_aborts_transaction_without_label(client, coll):
    coll.insert_one({"_id": "dup"})
    with client.start_session() as s:
        s.start_transaction()
        with pytest.raises(OperationFailure) as excinfo:
            coll.insert_one({"_id": "dup"}, session=s)
        assert not excinfo.value.has_error_label("TransientTransactionError")
        # Statement failure aborted the txn: commit gets 251 + label.
        with pytest.raises(OperationFailure) as commit_err:
            s.commit_transaction()
        assert commit_err.value.code == 251
        assert commit_err.value.has_error_label("TransientTransactionError")


def test_transaction_spanning_collections(client):
    db = client["txndb"]
    db["a"].insert_one({"_id": "seed-a"})
    db["b"].insert_one({"_id": "seed-b"})
    with client.start_session() as s, s.start_transaction():
        db["a"].insert_one({"_id": 1}, session=s)
        db["b"].insert_one({"_id": 2}, session=s)
    assert db["a"].count_documents({"_id": 1}) == 1
    assert db["b"].count_documents({"_id": 2}) == 1


def test_find_and_modify_and_distinct_in_transaction(client, coll):
    coll.insert_many([{"_id": i, "k": i % 2} for i in range(4)])
    with client.start_session() as s, s.start_transaction():
        doc = coll.find_one_and_update({"_id": 0}, {"$set": {"k": 99}}, session=s)
        assert doc["_id"] == 0
        # _id 0 flipped to 99; _id 1-3 keep k = 1, 0, 1.
        assert sorted(coll.distinct("k", session=s)) == [0, 1, 99]
    assert coll.find_one({"_id": 0})["k"] == 99


def test_getmore_inside_transaction(client, coll):
    coll.insert_many([{"_id": i} for i in range(10)])
    with client.start_session() as s, s.start_transaction():
        got = [d["_id"] for d in coll.find({}, batch_size=2, session=s)]
    assert got == list(range(10))


def test_change_stream_sees_committed_txn_with_lsid(client, coll):
    coll.insert_one({"_id": "pre"})
    with coll.watch() as stream:
        with client.start_session() as s, s.start_transaction():
            coll.insert_one({"_id": 1}, session=s)
            coll.insert_one({"_id": 2}, session=s)
        first = stream.next()
        second = stream.next()
    assert [first["documentKey"]["_id"], second["documentKey"]["_id"]] == [1, 2]
    # One shared commit timestamp + session/txn identity on both events.
    assert first["clusterTime"] == second["clusterTime"]
    assert first["lsid"] == second["lsid"]
    assert first["txnNumber"] == second["txnNumber"]


def test_change_stream_sees_nothing_from_aborted_txn(client, coll):
    coll.insert_one({"_id": "pre"})
    with coll.watch() as stream:
        with client.start_session() as s:
            s.start_transaction()
            coll.insert_one({"_id": "ghost"}, session=s)
            s.abort_transaction()
        coll.insert_one({"_id": "real"})
        event = stream.next()
    assert event["documentKey"]["_id"] == "real"
    assert "lsid" not in event
    assert "txnNumber" not in event


def test_snapshot_read_concern_accepted_in_transaction(client, coll):
    coll.insert_one({"_id": 1})
    with (
        client.start_session() as s,
        s.start_transaction(read_concern=pymongo.read_concern.ReadConcern("snapshot")),
    ):
        assert coll.find_one({"_id": 1}, session=s) is not None
    # Outside a transaction, snapshot is accepted on the
    # snapshot-readable commands too (replica-set persona, mongod 5.0+
    # semantics) — the reply pins the session via cursor.atClusterTime.
    reply = client["txndb"].command({"find": "things", "readConcern": {"level": "snapshot"}})
    assert reply["cursor"]["atClusterTime"] is not None


def test_transactions_on_separate_sessions_are_independent(client, coll):
    coll.insert_one({"_id": "seed2"})
    with client.start_session() as s1, client.start_session() as s2:
        s1.start_transaction()
        s2.start_transaction()
        coll.insert_one({"_id": "s1"}, session=s1)
        coll.insert_one({"_id": "s2"}, session=s2)
        s1.commit_transaction()
        s2.abort_transaction()
    ids = {d["_id"] for d in coll.find()}
    assert "s1" in ids
    assert "s2" not in ids


def test_non_txn_writer_retries_until_commit(client, coll):
    import threading
    import time

    coll.insert_one({"_id": 1, "v": 0})
    done = threading.Event()

    def plain_write():
        # Separate Python thread → separate pooled connection → separate
        # server thread, where the bounded conflict-retry loop spins.
        coll.update_one({"_id": 1}, {"$set": {"w": "plain"}})
        done.set()

    with client.start_session() as s:
        s.start_transaction()
        coll.update_one({"_id": 1}, {"$set": {"v": "txn"}}, session=s)
        t = threading.Thread(target=plain_write)
        t.start()
        time.sleep(0.4)
        # Still parked in the retry loop while the txn holds the write.
        assert not done.is_set()
        s.commit_transaction()
        t.join(timeout=5)
    assert done.is_set()
    doc = coll.find_one({"_id": 1})
    assert doc["v"] == "txn"
    assert doc["w"] == "plain"


def test_expired_transaction_is_reaped(tmp_path):
    import time

    with SecantusDBServer(
        port=0, storage_path=str(tmp_path), transaction_lifetime_seconds=0.5
    ) as srv:
        mc = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            c = mc["txndb"]["things"]
            c.insert_one({"_id": "seed"})
            with mc.start_session() as s:
                s.start_transaction()
                c.insert_one({"_id": "doomed"}, session=s)
                time.sleep(1.0)
                # The reaper aborted it; commit reports NoSuchTransaction
                # with the transient label so drivers retry the whole txn.
                with pytest.raises(OperationFailure) as excinfo:
                    s.commit_transaction()
                assert excinfo.value.code == 251
                assert excinfo.value.has_error_label("TransientTransactionError")
            assert c.count_documents({"_id": "doomed"}) == 0
        finally:
            mc.close()


def test_end_sessions_aborts_open_transaction(client, coll):
    coll.insert_one({"_id": "seed3"})
    s = client.start_session()
    s.start_transaction()
    coll.insert_one({"_id": "doomed"}, session=s)
    lsid = s.session_id
    client.admin.command({"endSessions": [lsid]})
    # The transaction was aborted server-side with the session.
    with pytest.raises(OperationFailure) as excinfo:
        s.commit_transaction()
    assert excinfo.value.code == 251
    assert coll.count_documents({"_id": "doomed"}) == 0


def test_server_stop_with_open_transaction(tmp_path):
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path))
    srv.start()
    mc = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
    try:
        c = mc["txndb"]["things"]
        c.insert_one({"_id": "seed"})
        s = mc.start_session()
        s.start_transaction()
        c.insert_one({"_id": "uncommitted"}, session=s)
    finally:
        mc.close()
    srv.stop()  # must not hang or leak; rolls the txn back
    # Reopen: the uncommitted write is gone.
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv2:
        mc2 = MongoClient(srv2.uri, serverSelectionTimeoutMS=2000)
        try:
            ids = {d["_id"] for d in mc2["txndb"]["things"].find()}
            assert ids == {"seed"}
        finally:
            mc2.close()


def test_implicit_collection_create_in_transaction(client):
    db = client["txndb"]
    name = "made_in_txn"
    assert name not in db.list_collection_names()
    with client.start_session() as s, s.start_transaction():
        db[name].insert_one({"_id": 1}, session=s)
    assert name in db.list_collection_names()
    assert db[name].count_documents({}) == 1


def test_create_collection_and_index_in_transaction(client):
    db = client["txndb"]
    with client.start_session() as s, s.start_transaction():
        db.create_collection("ddl_in_txn", session=s)
        db["ddl_in_txn"].create_index("x", session=s)
        db["ddl_in_txn"].insert_one({"_id": 1, "x": 9}, session=s)
    assert "ddl_in_txn" in db.list_collection_names()
    names = {ix["name"] for ix in db["ddl_in_txn"].list_indexes()}
    assert "x_1" in names
    assert db["ddl_in_txn"].count_documents({"x": 9}) == 1


def test_aborted_ddl_rolls_back(client):
    db = client["txndb"]
    with client.start_session() as s:
        s.start_transaction()
        db.create_collection("ghost_coll", session=s)
        db["ghost_coll"].insert_one({"_id": 1}, session=s)
        s.abort_transaction()
    assert "ghost_coll" not in db.list_collection_names()
    # The namespace is fully reusable after the rollback.
    db["ghost_coll"].insert_one({"_id": "fresh"})
    assert db["ghost_coll"].count_documents({}) == 1


def test_transaction_too_large_for_cache(tmp_path):
    # An oversized multi-document transaction is rejected with mongod's
    # TransactionTooLargeForCache (313) BEFORE its unevictable dirty
    # content can stall the storage engine. Not transient — retrying the
    # same transaction would hit the same wall — and the failed statement
    # aborts the transaction server-side (mongod parity).
    with SecantusDBServer(port=0, storage_path=str(tmp_path), cache_size="128M") as srv:
        client = MongoClient(f"mongodb://127.0.0.1:{srv.address[1]}/")
        try:
            coll = client.txndb.big
            docs = [{"_id": i, "pad": "x" * (1024 * 1024)} for i in range(16)]
            with client.start_session() as sess:
                sess.start_transaction()
                with pytest.raises(OperationFailure) as exc_info:
                    coll.insert_many(docs, session=sess)
                assert exc_info.value.code == 313
                assert "TransientTransactionError" not in (
                    exc_info.value.details.get("errorLabels") or []
                )
                sess.abort_transaction()
            # Nothing from the aborted transaction is visible.
            assert coll.count_documents({}) == 0
            # The same payload outside a transaction inserts fine.
            coll.insert_many(docs)
            assert coll.count_documents({}) == 16
        finally:
            client.close()


def test_write_concern_inside_transaction_is_rejected(client) -> None:
    """A per-operation ``writeConcern`` inside a transaction is InvalidOptions.

    A transaction's write concern is fixed at commit time, so mongod refuses
    one on a statement (code 72). We accepted and silently ignored it, letting
    a caller believe a statement ran at a concern it did not.

    Drivers guard this client-side — the transactions spec marks these cases
    ``isClientError: true`` — so no driver gauge exercises it; this is the
    raw-command path, which is exactly where fidelity is otherwise untested.
    """
    from pymongo.errors import OperationFailure

    db = client["txn_wc"]
    db.create_collection("c")
    with client.start_session() as sess:
        sess.start_transaction()
        with pytest.raises(OperationFailure) as exc:
            db.command(
                {
                    "insert": "c",
                    "documents": [{"_id": 1}],
                    "writeConcern": {"w": 1},
                },
                session=sess,
            )
        assert exc.value.code == 72
        assert "Cannot set write concern after starting a transaction" in str(exc.value)
        sess.abort_transaction()


def test_read_concern_on_a_continuing_statement_is_rejected(client) -> None:
    """``readConcern`` may ride only the statement that STARTS a transaction."""
    from pymongo.errors import OperationFailure

    db = client["txn_rc"]
    db.create_collection("c")
    with client.start_session() as sess:
        sess.start_transaction()
        db.command({"insert": "c", "documents": [{"_id": 1}]}, session=sess)
        with pytest.raises(OperationFailure) as exc:
            db.command(
                {"find": "c", "readConcern": {"level": "local"}},
                session=sess,
            )
        assert exc.value.code == 72
        assert "Cannot set read concern after starting a transaction" in str(exc.value)
        sess.abort_transaction()


def test_read_concern_on_the_first_statement_is_allowed(client) -> None:
    """The starting statement MAY carry a readConcern — that is how a
    transaction's read concern gets chosen at all.

    Guards the over-rejection direction: a blanket ban would break every
    transaction that sets a read concern, which is the normal way to open one.
    """
    db = client["txn_rc_ok"]
    db.create_collection("c")
    with client.start_session() as sess:
        with sess.start_transaction(read_concern=ReadConcern("local")):
            db.c.insert_one({"_id": 1}, session=sess)
        assert db.c.count_documents({}) == 1
