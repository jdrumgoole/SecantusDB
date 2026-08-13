"""Retryable-write idempotency.

A driver retries a write with the SAME ``lsid`` + ``txnNumber`` after a
network blip, a ``writeConcernError``, or a stepdown. mongod persists the
statement's outcome (``config.transactions``) and replays it rather than
executing the write twice. Without that, a retried ``{$inc: {n: 1}}``
increments twice while both replies claim ``nModified: 1`` — silent data
corruption on a path every official driver exercises automatically.
"""

from __future__ import annotations

import bson
import pytest
from pymongo import MongoClient

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


def _retryable(db, cmd: dict, lsid, txn: int) -> dict:
    """Send ``cmd`` as a retryable write with an explicit lsid/txnNumber."""
    full = dict(cmd)
    full["lsid"] = lsid
    full["txnNumber"] = bson.Int64(txn)
    return db.command(full)


def test_retried_inc_applies_once(client: MongoClient) -> None:
    """The silent case: a non-idempotent operator must not double-apply."""
    db = client["rw_inc"]
    db.c.insert_one({"_id": 1, "n": 0})
    with client.start_session() as sess:
        cmd = {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$inc": {"n": 1}}}]}
        first = _retryable(db, cmd, sess.session_id, 1)
        retry = _retryable(db, cmd, sess.session_id, 1)
    assert first["nModified"] == 1
    # The replay is the stored reply, so the client sees the same answer.
    assert retry["nModified"] == 1
    assert db.c.find_one({"_id": 1})["n"] == 1, "retried $inc applied twice"


def test_retried_insert_is_not_a_duplicate_key_error(client: MongoClient) -> None:
    """The loud case: the retry must not collide with its own first attempt."""
    db = client["rw_insert"]
    with client.start_session() as sess:
        cmd = {"insert": "c", "documents": [{"_id": 7, "x": 1}]}
        first = _retryable(db, cmd, sess.session_id, 1)
        retry = _retryable(db, cmd, sess.session_id, 1)
    assert first["n"] == 1
    assert retry["n"] == 1
    assert not retry.get("writeErrors"), f"retry re-executed: {retry.get('writeErrors')}"
    assert db.c.count_documents({}) == 1


def test_retried_push_applies_once(client: MongoClient) -> None:
    """``$push`` is the other operator that corrupts quietly on replay."""
    db = client["rw_push"]
    db.c.insert_one({"_id": 1, "xs": []})
    with client.start_session() as sess:
        cmd = {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$push": {"xs": "a"}}}]}
        _retryable(db, cmd, sess.session_id, 1)
        _retryable(db, cmd, sess.session_id, 1)
    assert db.c.find_one({"_id": 1})["xs"] == ["a"]


def test_a_new_txn_number_executes_again(client: MongoClient) -> None:
    """A DIFFERENT txnNumber is a new write, not a retry — it must apply.

    The failure mode this guards is over-caching: keying too loosely (on the
    session alone, say) would make a second genuine write vanish.
    """
    db = client["rw_seq"]
    db.c.insert_one({"_id": 1, "n": 0})
    with client.start_session() as sess:
        cmd = {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$inc": {"n": 1}}}]}
        _retryable(db, cmd, sess.session_id, 1)
        _retryable(db, cmd, sess.session_id, 2)
    assert db.c.find_one({"_id": 1})["n"] == 2


def test_registry_isolates_sessions_and_verifies_identity() -> None:
    """Record lookup is keyed on (lsid, txnNumber) AND the command identity.

    Driven against the registry directly rather than over the wire: pymongo
    OVERRIDES an explicit ``lsid`` on ``db.command()`` with its own implicit
    session, so a wire-level test cannot actually present two distinct
    sessions (verified — the server received one lsid for both commands).
    Session isolation is registry logic, so that is where it is pinned.
    """
    from secantus.transactions import TransactionRegistry

    reg = TransactionRegistry()
    ident = b"\x01" * 20
    other = b"\x02" * 20
    reply = {"ok": 1.0, "n": 1}

    reg.record_retryable(b"session-a", 1, ident, reply)

    # Same session, same number, same command -> replay.
    assert reg.retryable_reply(b"session-a", 1, ident) == reply
    # Different session -> not a retry.
    assert reg.retryable_reply(b"session-b", 1, ident) is None
    # Different txnNumber -> a new write.
    assert reg.retryable_reply(b"session-a", 2, ident) is None
    # Same key, DIFFERENT command: replaying here would serve one write's
    # answer for another, which is worse than the double-apply this prevents.
    assert reg.retryable_reply(b"session-a", 1, other) is None


def test_registry_does_not_record_failed_writes() -> None:
    """Only writes that took effect are replayable."""
    from secantus.transactions import TransactionRegistry

    reg = TransactionRegistry()
    ident = b"\x03" * 20

    reg.record_retryable(b"s", 1, ident, {"ok": 0.0, "errmsg": "boom", "code": 1})
    assert reg.retryable_reply(b"s", 1, ident) is None

    reg.record_retryable(b"s", 2, ident, {"ok": 1.0, "writeErrors": [{"index": 0}]})
    assert reg.retryable_reply(b"s", 2, ident) is None

    # A writeConcernError means the write DID apply — replication of it did
    # not confirm. mongod records it, so a retry must not apply it twice.
    reg.record_retryable(b"s", 3, ident, {"ok": 1.0, "n": 1, "writeConcernError": {"code": 64}})
    assert reg.retryable_reply(b"s", 3, ident) is not None


def test_registry_expires_records() -> None:
    """Records age out, so a much later retry re-executes as mongod's does."""
    from secantus.transactions import TransactionRegistry

    now = [1000.0]
    reg = TransactionRegistry(time_func=lambda: now[0])
    ident = b"\x04" * 20
    reg.record_retryable(b"s", 1, ident, {"ok": 1.0, "n": 1})
    assert reg.retryable_reply(b"s", 1, ident) is not None

    now[0] += 31 * 60  # past the 30-minute lifetime
    assert reg.retryable_reply(b"s", 1, ident) is None


def test_a_failed_write_is_not_recorded(client: MongoClient) -> None:
    """A write that failed did not take effect, so the retry must re-run it.

    Caching failures would turn a transient error into a permanent one: the
    client retries, gets the stored failure back, and can never succeed.
    """
    db = client["rw_fail"]
    db.c.insert_one({"_id": 1})
    with client.start_session() as sess:
        cmd = {"insert": "c", "documents": [{"_id": 1}]}  # duplicate _id
        first = _retryable(db, cmd, sess.session_id, 1)
        assert first.get("writeErrors"), "expected a duplicate-key writeError"
        # Remove the conflict; the retry must genuinely execute now.
        db.c.delete_one({"_id": 1})
        retry = _retryable(db, cmd, sess.session_id, 1)
    assert not retry.get("writeErrors"), "a failed write must not be replayed as failed"
    assert db.c.count_documents({"_id": 1}) == 1


def test_reads_are_not_cached(client: MongoClient) -> None:
    """Only writes get records; a stray txnNumber on a find must not freeze it."""
    db = client["rw_read"]
    db.c.insert_one({"_id": 1, "n": 0})
    with client.start_session() as sess:
        out1 = db.command(
            {"find": "c", "filter": {}, "lsid": sess.session_id, "txnNumber": bson.Int64(1)}
        )
        assert out1["cursor"]["firstBatch"][0]["n"] == 0
        db.c.update_one({"_id": 1}, {"$set": {"n": 42}})
        out2 = db.command(
            {"find": "c", "filter": {}, "lsid": sess.session_id, "txnNumber": bson.Int64(1)}
        )
    assert out2["cursor"]["firstBatch"][0]["n"] == 42, "read replayed a stale cached reply"
