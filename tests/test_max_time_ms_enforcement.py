"""``maxTimeMS`` is ENFORCED, not just validated.

The value was parsed exactly as mongod parses it and then ignored: the operation
ran to completion and answered ``ok`` where mongod aborts it with
``50 MaxTimeMSExpired``. Measured against mongod 8.2.11 on 2026-09-01 -- a sweep
at ``maxTimeMS: 1000`` showed nothing (nothing is slow enough) and one at
``maxTimeMS: 2`` showed the divergence on find / count / distinct / aggregate /
update / createIndexes.

These tests use a budget of 1 ms against a collection big enough that the scan
cannot finish inside it, and a generous budget to prove the deadline is not
armed when it should not be. Timing-dependent by nature -- the seeded collection
is sized so the margin is three orders of magnitude, not two.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import ExecutionTimeout, OperationFailure

from secantus import SecantusDBServer


@pytest.fixture(scope="module")
def server(wt_home_module):
    with SecantusDBServer(port=0, storage_path=wt_home_module) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=5000)
    try:
        yield c
    finally:
        c.close()


#: Big enough that a full scan cannot finish in a millisecond on any machine
#: this suite runs on, small enough that seeding it is not the slow part.
_ROWS = 40000


@pytest.fixture()
def loaded(client):
    coll = client["mtms_db"]["big"]
    coll.drop()
    for start in range(0, _ROWS, 10000):
        coll.insert_many(
            [{"_id": i, "a": i % 97, "s": "x" * 200} for i in range(start, start + 10000)]
        )
    return coll


def _expired(exc: OperationFailure) -> bool:
    return exc.details.get("code") == 50 and exc.details.get("codeName") == "MaxTimeMSExpired"


@pytest.mark.parametrize(
    "command",
    [
        {"find": "big", "filter": {"a": {"$gt": -1}}},
        {"count": "big", "query": {"a": {"$gt": -1}}},
        {"distinct": "big", "key": "s"},
        {
            "aggregate": "big",
            "pipeline": [{"$match": {"a": {"$gt": -1}}}, {"$group": {"_id": "$a"}}],
            "cursor": {},
        },
        {
            "update": "big",
            "updates": [{"q": {"a": {"$gt": -1}}, "u": {"$inc": {"z": 0}}, "multi": True}],
        },
        {"delete": "big", "deletes": [{"q": {"a": {"$gt": -1}}, "limit": 0}]},
    ],
    ids=["find", "count", "distinct", "aggregate", "update", "delete"],
)
def test_an_exhausted_budget_answers_50(loaded, command):
    db = loaded.database
    with pytest.raises((OperationFailure, ExecutionTimeout)) as exc:
        db.command({**command, "maxTimeMS": 1})
    assert _expired(exc.value)
    assert exc.value.details["errmsg"] == "operation exceeded time limit"


def test_create_indexes_wraps_the_timeout_in_an_index_build_failure(loaded):
    """mongod's own envelope: the build uuid, the namespace, the collection
    uuid, then the cause."""
    db = loaded.database
    with pytest.raises((OperationFailure, ExecutionTimeout)) as exc:
        db.command(
            {
                "createIndexes": "big",
                "indexes": [{"key": {"s": 1}, "name": "s_1"}],
                "maxTimeMS": 1,
            }
        )
    assert _expired(exc.value)
    message = exc.value.details["errmsg"]
    assert message.startswith("Index build failed: ")
    assert "Collection mtms_db.big (" in message
    assert message.endswith(":: caused by :: operation exceeded time limit")


def test_a_generous_budget_does_not_fire(loaded):
    assert loaded.database.command({"count": "big", "query": {"a": 1}, "maxTimeMS": 60000})["n"]


def test_an_absent_or_zero_budget_arms_nothing(loaded):
    """mongod encodes "no limit" as an absent field OR an explicit 0."""
    db = loaded.database
    assert db.command({"count": "big", "query": {"a": 1}})["n"]
    assert db.command({"count": "big", "query": {"a": 1}, "maxTimeMS": 0})["n"]


def test_the_deadline_does_not_leak_into_the_next_command(loaded):
    """It is armed per command; a connection that just timed out must be able
    to run the same operation again with no budget."""
    db = loaded.database
    with pytest.raises((OperationFailure, ExecutionTimeout)):
        db.command({"count": "big", "query": {"a": {"$gt": -1}}, "maxTimeMS": 1})
    assert db.command({"count": "big", "query": {"a": {"$gt": -1}}})["n"] == _ROWS
