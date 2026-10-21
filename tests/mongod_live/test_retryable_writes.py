"""Retryable writes against a one-member mongod 8.2 replica set.

A driver retries a write once, with the same `lsid` and `txnNumber`, when
it did not hear the answer; the server must answer the retry as it answered
the first attempt and apply the write once. No CI test compared this with
mongod (the coverage review of 2026-10-21). Each case sends raw commands
with an explicit session, so the retry is exact and nothing depends on a
driver's retry logic.
"""

from __future__ import annotations

import re
from collections.abc import Callable
from typing import Any

import pymongo
import pytest
from bson import Int64

from tests.mongod_live.conftest import mongo_answer, same

#: Reply fields that are per-server by nature.
_PER_SERVER = {"operationTime", "$clusterTime", "electionId", "opTime", "lastErrorObject_upserted"}


def _reply(answer: tuple) -> tuple:
    if answer[0] != "ok":
        # The message names the session, which is per-server.
        return (*answer[:2], re.sub(r"on session \S+ - \S+ ", "on session S ", answer[2]))
    return ("ok", {k: v for k, v in answer[1].items() if k not in _PER_SERVER})


class _Txn:
    """An explicit session, with txnNumbers counted from `base`. pymongo
    pools server sessions, so a session can arrive with txnNumbers already
    used; each case's base is above every earlier case's, so its numbers
    are always the newest, and they are the same on both servers."""

    def __init__(self, db: Any, session: Any, base: int) -> None:
        self.db, self.session, self.base = db, session, base

    def __call__(self, txn: int, cmd: dict) -> tuple:
        body = {**cmd, "txnNumber": Int64(self.base + txn)}
        return _reply(mongo_answer(lambda: self.db.command(body, session=self.session)))


def _run(client: Any, steps: Callable[[_Txn], list], base: int) -> list:
    client.drop_database("live_retry")
    db = client["live_retry"]
    db.c.insert_many([{"_id": 1, "n": 0}, {"_id": 2, "n": 0}])
    # A client of its own, so the server session these txnNumbers advance
    # never goes back to the shared client's pool, whose own retryable
    # writes would then be "too old" on it.
    host, port = client.address
    own = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=10_000)
    try:
        with own.start_session() as session:
            replies = steps(_Txn(own["live_retry"], session, base))
    finally:
        own.close()
    return [*replies, ("state", list(db.c.find().sort("_id", 1)))]


def _insert_twice(t: _Txn) -> list:
    cmd = {"insert": "c", "documents": [{"_id": 3}, {"_id": 4}]}
    return [t(1, cmd), t(1, cmd)]


def _inc_twice(t: _Txn) -> list:
    cmd = {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$inc": {"n": 1}}}]}
    return [t(1, cmd), t(1, cmd)]


def _upsert_twice(t: _Txn) -> list:
    cmd = {"update": "c", "updates": [{"q": {"_id": 9}, "u": {"$inc": {"n": 1}}, "upsert": True}]}
    return [t(1, cmd), t(1, cmd)]


def _delete_twice(t: _Txn) -> list:
    cmd = {"delete": "c", "deletes": [{"q": {}, "limit": 1}]}
    return [t(1, cmd), t(1, cmd)]


def _find_and_modify_twice(t: _Txn) -> list:
    cmd = {"findAndModify": "c", "query": {"_id": 2}, "update": {"$inc": {"n": 5}}, "new": True}
    return [t(1, cmd), t(1, cmd)]


def _batch_retried_in_part(t: _Txn) -> list:
    """A two-statement batch, then its retry: both statements already ran."""
    cmd = {
        "update": "c",
        "updates": [
            {"q": {"_id": 1}, "u": {"$inc": {"n": 1}}},
            {"q": {"_id": 2}, "u": {"$inc": {"n": 1}}},
        ],
    }
    return [t(1, cmd), t(1, cmd)]


def _then_next_txn(t: _Txn) -> list:
    """A new txnNumber applies; an older one is refused."""
    inc = {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$inc": {"n": 1}}}]}
    return [t(5, inc), t(6, inc), t(5, inc)]


def _same_txn_other_statement(t: _Txn) -> list:
    """The same txnNumber with a different statement: mongod answers from
    the statement ids it has already executed, not from the new body."""
    a = {"update": "c", "updates": [{"q": {"_id": 1}, "u": {"$inc": {"n": 1}}}]}
    b = {"update": "c", "updates": [{"q": {"_id": 2}, "u": {"$inc": {"n": 100}}}]}
    return [t(1, a), t(1, b)]


CASES = [
    ("insert", _insert_twice),
    ("update", _inc_twice),
    ("upsert", _upsert_twice),
    ("delete", _delete_twice),
    ("findAndModify", _find_and_modify_twice),
    ("two-statement batch", _batch_retried_in_part),
    ("later and earlier txnNumber", _then_next_txn),
    ("same txnNumber, other statement", _same_txn_other_statement),
]


@pytest.mark.parametrize(
    ("base", "name", "steps"),
    [(1000 * (i + 1), n, f) for i, (n, f) in enumerate(CASES)],
    ids=[n for n, _ in CASES],
)
def test_retry_matches_mongod(
    base: int, name: str, steps: Callable, rust_mongo: Any, mongod_rs: Any
) -> None:
    same(_run(rust_mongo, steps, base), _run(mongod_rs, steps, base))
