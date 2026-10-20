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
        first = rust_mongo.start_session()
        second = rust_mongo.start_session()
        first.start_transaction()
        db.create_collection("n", session=first)
        second.start_transaction()
        waiting = threading.Thread(
            target=lambda: mongo_answer(lambda: db.command("create", "n", session=second)),
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
        first.abort_transaction()
        waiting.join(10)
        again.join(10)
        assert not waiting.is_alive() and not again.is_alive()
    finally:
        admin.command("setParameter", 1, maxTransactionLockRequestTimeoutMillis=5)


@pytest.mark.backlog_fixed("RM-201")
def test_a_transaction_lock_request_times_out(rust_mongo: Any) -> None:
    """mongod 8.2.11 gives a transaction's lock request
    `maxTransactionLockRequestTimeoutMillis` (5 ms) and then fails it with
    24 LockTimeout, labelled TransientTransactionError."""
    db = rust_mongo["rm201b"]
    db.drop_collection("n")
    first = rust_mongo.start_session()
    second = rust_mongo.start_session()
    first.start_transaction()
    db.create_collection("n", session=first)
    second.start_transaction()
    began = time.monotonic()
    got = mongo_answer(lambda: db.command("create", "n", session=second))
    assert time.monotonic() - began < 2
    assert got[:2] == ("err", 24)
    first.abort_transaction()
    assert (
        rust_mongo.admin.command("getParameter", 1, maxTransactionLockRequestTimeoutMillis=1)[
            "maxTransactionLockRequestTimeoutMillis"
        ]
        == 5
    )


@pytest.mark.backlog_fixed("RM-138")
def test_a_waiting_drop_is_not_overtaken(rust_mongo: Any) -> None:
    """Lock requests are granted first come first served, as mongod's lock
    queue grants them: with a transaction holding a collection and a `drop`
    waiting on it, a new transaction's insert queues behind the `drop` and
    times out (mongod 8.2.11: 24 LockTimeout) rather than overtaking it. A
    `drop` behind a stream of writers used to wait for ever (RM-138)."""
    db = rust_mongo["rm138"]
    db.drop_collection("w")
    db.w.insert_one({"_id": 0})
    holder = rust_mongo.start_session()
    holder.start_transaction()
    db.w.insert_one({"_id": 1}, session=holder)
    dropping = threading.Thread(
        target=lambda: mongo_answer(lambda: db.command("drop", "w", maxTimeMS=3000)),
        daemon=True,
    )
    dropping.start()
    time.sleep(0.3)
    late = rust_mongo.start_session()
    late.start_transaction()
    got = mongo_answer(lambda: db.w.insert_one({"_id": 2}, session=late))
    assert got[:2] == ("err", 24)
    mongo_answer(late.abort_transaction)
    holder.abort_transaction()
    dropping.join(10)
    assert not dropping.is_alive()
