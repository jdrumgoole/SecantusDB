"""Transactions and the lock manager (RM-201)."""

from __future__ import annotations

import threading
import time
from typing import Any

import pymongo
import pytest

from tests.backlog.conftest import mongo_answer


@pytest.mark.backlog_fixed("RM-201")
def test_a_transaction_waiting_on_a_lock_leaves_the_server_answering(rust_mongo: Any) -> None:
    """A transaction statement waiting for another transaction's lock held
    its own transaction's mutex, and a second command on that session then
    waited for it holding the session registry, which every command and the
    accept loop need: the whole server stopped answering (found 2026-10-20
    by `tools/probes/transactions.py`). With no lock timeout the wait goes
    on, but nothing else may wait with it."""
    admin = rust_mongo.admin
    admin.command("setParameter", 1, maxTransactionLockRequestTimeoutMillis=-1)
    try:
        db = rust_mongo["rm201"]
        db.drop_collection("n")
        db.n.insert_one({"_id": 0})
        first = rust_mongo.start_session()
        second = rust_mongo.start_session()
        first.start_transaction()
        db.n.insert_one({"_id": 1}, session=first)
        # A `drop` queues for the collection behind the first transaction,
        # and the second transaction's insert queues behind the drop: with
        # no lock timeout it waits for the first transaction to end.
        host, port = rust_mongo.address
        other = pymongo.MongoClient(host, port, directConnection=True)
        dropper = threading.Thread(
            target=lambda: mongo_answer(lambda: other.rm201.command("drop", "n")),
            daemon=True,
        )
        dropper.start()
        time.sleep(0.3)
        second.start_transaction()
        waiting = threading.Thread(
            target=lambda: mongo_answer(lambda: db.n.insert_one({"_id": 2}, session=second)),
            daemon=True,
        )
        waiting.start()
        time.sleep(0.3)
        again = threading.Thread(
            target=lambda: mongo_answer(lambda: db.n.find_one({}, session=second)),
            daemon=True,
        )
        again.start()
        time.sleep(0.3)
        host, port = rust_mongo.address
        fresh = pymongo.MongoClient(host, port, serverSelectionTimeoutMS=3000)
        try:
            assert fresh.admin.command("ping")["ok"] == 1.0
        finally:
            fresh.close()
        assert waiting.is_alive(), "the second transaction's insert did not wait"
        first.abort_transaction()
        waiting.join(10)
        again.join(10)
        dropper.join(10)
        other.close()
        assert not waiting.is_alive() and not again.is_alive()
    finally:
        admin.command("setParameter", 1, maxTransactionLockRequestTimeoutMillis=5)


@pytest.mark.backlog_fixed("RM-201")
def test_a_transaction_lock_request_times_out(rust_mongo: Any) -> None:
    """mongod 8.2.11 gives a transaction's lock request
    `maxTransactionLockRequestTimeoutMillis` (5 ms) and then fails it with
    24 LockTimeout, labelled TransientTransactionError: here a second
    transaction's insert queued behind a `drop` that waits for the first
    transaction to end (measured 2026-10-20; the `drop` then runs once the
    first aborts). A second transaction CREATING the same collection is no
    such case: mongod lets it, and reports the clash at commit (RM-202)."""
    db = rust_mongo["rm201b"]
    db.drop_collection("n")
    db.n.insert_one({"_id": 0})
    first = rust_mongo.start_session()
    second = rust_mongo.start_session()
    first.start_transaction()
    db.n.insert_one({"_id": 1}, session=first)
    host, port = rust_mongo.address
    other = pymongo.MongoClient(host, port, directConnection=True)
    dropped: list = []
    dropper = threading.Thread(
        target=lambda: dropped.append(mongo_answer(lambda: other.rm201b.command("drop", "n"))),
        daemon=True,
    )
    dropper.start()
    time.sleep(0.5)
    second.start_transaction()
    began = time.monotonic()
    got = mongo_answer(lambda: db.n.insert_one({"_id": 2}, session=second))
    assert time.monotonic() - began < 2
    assert got[:2] == ("err", 24)
    first.abort_transaction()
    dropper.join(10)
    other.close()
    assert dropped and dropped[0][0] == "ok"
    assert (
        rust_mongo.admin.command("getParameter", 1, maxTransactionLockRequestTimeoutMillis=1)[
            "maxTransactionLockRequestTimeoutMillis"
        ]
        == 5
    )
