"""``apiStrict: true`` rejects the small set of commands the Stable
API v1 spec explicitly probes for.

Mongod's Stable API rejects a list of commands when ``apiStrict:
true`` is set. SecantusDB ships a narrow whitelist of rejected
names rather than the full mongod inverse — only the commands the
spec's unified test runners actively probe:

* ``distinct`` — mongo-java-driver's
  ``crud-api-version-1-strict.yml`` test ``distinct appends
  declared API version`` asserts ``errorCodeName: APIStrictError``.

The aggregation-stage gate (rejecting non-v1 stages inside an
``aggregate`` pipeline) is exercised by
``tests/test_api_version.py`` (pre-existing). This file pins the
new command-name gate's wire behaviour.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield c
    finally:
        c.close()


def test_distinct_rejected_under_api_strict(client) -> None:
    """``distinct`` with ``apiVersion: 1, apiStrict: true`` returns
    code 323 (``APIStrictError``). The wire response shape matches
    mongod's so the unified test runner's ``errorCodeName`` assertion
    matches."""
    coll = client["db"]["coll"]
    coll.insert_many([{"_id": 1, "x": "a"}, {"_id": 2, "x": "b"}])

    with pytest.raises(OperationFailure) as exc_info:
        client["db"].command({"distinct": "coll", "key": "x", "apiVersion": "1", "apiStrict": True})
    assert exc_info.value.code == 323
    assert exc_info.value.details.get("codeName") == "APIStrictError"
    assert "distinct" in exc_info.value.details.get("errmsg", "")


def test_distinct_allowed_without_api_strict(client) -> None:
    """``distinct`` without ``apiStrict: true`` works normally.
    ``apiVersion: 1`` alone (no ``apiStrict``) doesn't restrict the
    surface — drivers tag every command with ``apiVersion`` once a
    ``ServerApi`` is set on the client; the restriction is opt-in
    via ``apiStrict``."""
    coll = client["db"]["coll"]
    coll.insert_many([{"_id": 1, "x": "a"}, {"_id": 2, "x": "b"}])

    res = client["db"].command({"distinct": "coll", "key": "x", "apiVersion": "1"})
    assert sorted(res["values"]) == ["a", "b"]

    # And bare distinct (no apiVersion at all) works too.
    assert sorted(coll.distinct("x")) == ["a", "b"]


def test_count_still_allowed_under_api_strict(client) -> None:
    """``count`` is NOT in the rejected set — the Java driver's
    ``estimatedDocumentCount`` helper sends ``count`` under
    ``apiStrict: true``, and mongod tolerates it for back-compat.
    Rejecting it would cascade-fail unrelated tests."""
    coll = client["db"]["coll"]
    coll.insert_many([{"_id": i} for i in range(5)])

    res = client["db"].command({"count": "coll", "apiVersion": "1", "apiStrict": True})
    assert res["n"] == 5


def test_find_still_allowed_under_api_strict(client) -> None:
    """``find`` is a v1 command; ``apiStrict: true`` doesn't affect it."""
    coll = client["db"]["coll"]
    coll.insert_one({"_id": 1, "v": "ok"})

    res = client["db"].command({"find": "coll", "filter": {}, "apiVersion": "1", "apiStrict": True})
    assert res["cursor"]["firstBatch"] == [{"_id": 1, "v": "ok"}]


def test_aggregate_with_v1_stage_allowed(client) -> None:
    """``aggregate`` with v1 stages works under ``apiStrict``.
    Confirms the command-name gate and the stage gate compose
    correctly — ``aggregate`` itself is allowed; only non-v1 stages
    inside it would be rejected."""
    coll = client["db"]["coll"]
    coll.insert_many([{"v": 1}, {"v": 2}, {"v": 3}])

    res = client["db"].command(
        {
            "aggregate": "coll",
            "pipeline": [{"$match": {"v": {"$gt": 1}}}, {"$count": "n"}],
            "cursor": {},
            "apiVersion": "1",
            "apiStrict": True,
        }
    )
    assert res["cursor"]["firstBatch"] == [{"n": 2}]
