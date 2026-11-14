"""The query stats store's size, sampling and explains against mongod 8.2
(RM-532): `internalQueryStatsCacheSize` and `internalQueryStatsSampleRate`
as `setParameter` takes them, the store turned off at 0 bytes, its counters
in `serverStatus`, eviction when the budget shrinks, and the key of an
`aggregate` that explains.

Each test runs on servers of its own: what a store records outlives the
parameter that let it record, and a budget of 0 empties it.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import pymongo
import pytest
from bson import Int64
from bson.son import SON

from tests.backlog.conftest import rust_mongo_server
from tests.mongod_live.conftest import same

_DB = "qstore_live"


@pytest.fixture
def servers(tmp_path: Any) -> Iterator[list[pymongo.MongoClient]]:
    from tests.mongod_reference import MONGOD, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod()
    reference = pymongo.MongoClient(uri, serverSelectionTimeoutMS=10_000)
    try:
        with rust_mongo_server(tmp_path / "store") as ours:
            yield [ours, reference]
    finally:
        reference.close()
        _stop_mongod(proc, dbpath)


def _set(client: pymongo.MongoClient, **params: Any) -> Any:
    try:
        return client.admin.command("setParameter", 1, **params)["was"]
    except pymongo.errors.OperationFailure as e:
        return ("err", e.code, str(e.details.get("errmsg")))


def _ours(client: pymongo.MongoClient) -> list[dict]:
    """The entries of this module's queries, less the client's metadata."""
    out = []
    for e in client.admin.aggregate([{"$queryStats": {}}]):
        key = e["key"]
        if key["queryShape"]["cmdNs"].get("db") == _DB:
            key.pop("client", None)
            out.append(key)
    return sorted(out, key=str)


_SIZES: list[Any] = [
    "0MB",
    "0",
    "5MB",
    "1.5GB",
    " 2 kb ",
    "10B",
    "100%",
    "0%",
    "abc",
    "5TB",
    "",
    "-1MB",
    "1e3MB",
    5,
    5.5,
    True,
    None,
    "1%",
]


def test_the_cache_size_parameter(servers: list[pymongo.MongoClient]) -> None:
    """`internalQueryStatsCacheSize` is a string naming a size -- a number,
    then `B`, `KB`, `MB`, `GB` or `%` in any case -- reported as it was
    set; a value of another type does not coerce (2) and a string that does
    not parse is 6007012."""
    answers = []
    for client in servers:
        got = [
            client.admin.command("getParameter", 1, internalQueryStatsCacheSize=1)[
                "internalQueryStatsCacheSize"
            ]
        ]
        for size in _SIZES:
            was = _set(client, internalQueryStatsCacheSize=size)
            now = client.admin.command("getParameter", 1, internalQueryStatsCacheSize=1)
            got.append((was, now["internalQueryStatsCacheSize"]))
        got.append(
            client.admin.command(
                "getParameter", {"showDetails": True}, internalQueryStatsCacheSize=1
            )["internalQueryStatsCacheSize"]
        )
        answers.append(got)
    same(answers[0], answers[1])


@pytest.mark.parametrize("rate", [0.5, 0, 1, 1.5, -0.1, "0.5", True, Int64(1), 2, None], ids=repr)
def test_the_sample_rate_parameter(servers: list[pymongo.MongoClient], rate: Any) -> None:
    """`internalQueryStatsSampleRate` is any number from 0 to 1, kept as a
    double."""
    answers = []
    for client in servers:
        was = _set(client, internalQueryStatsSampleRate=rate)
        now = client.admin.command("getParameter", 1, internalQueryStatsSampleRate=1)
        answers.append((was, now["internalQueryStatsSampleRate"]))
    same(answers[0], answers[1])


def test_a_sample_rate_of_one_records_every_query(servers: list[pymongo.MongoClient]) -> None:
    """A sample rate above 0 decides in place of the rate limit: at 1 every
    query is recorded with the rate limit still at its default, 0."""
    answers = []
    for client in servers:
        client.admin.command("setParameter", 1, internalQueryStatsSampleRate=1)
        db = client[_DB]
        db.c.insert_many([{"a": i} for i in range(3)])
        list(db.c.find({"a": 1}))
        list(db.c.find({"b": {"$gt": 1}}))
        answers.append([k["queryShape"]["filter"] for k in _ours(client)])
    same(answers[0], answers[1])


def test_no_budget_turns_query_stats_off(servers: list[pymongo.MongoClient]) -> None:
    """At 0 bytes nothing is recorded and `$queryStats` fails (6579000);
    given a budget again, the store starts empty."""
    answers = []
    for client in servers:
        db = client[_DB]
        db.c.insert_one({"a": 1})
        client.admin.command("setParameter", 1, internalQueryStatsRateLimit=-1)
        list(db.c.find({"a": 1}))
        client.admin.command("setParameter", 1, internalQueryStatsCacheSize="0MB")
        list(db.c.find({"b": 1}))
        try:
            list(client.admin.aggregate([{"$queryStats": {}}]))
            listed: Any = "ok"
        except pymongo.errors.OperationFailure as e:
            listed = ("err", e.code, str(e.details.get("errmsg")))
        client.admin.command("setParameter", 1, internalQueryStatsCacheSize="1MB")
        answers.append((listed, _ours(client)))
    same(answers[0], answers[1])


def test_a_smaller_budget_evicts(servers: list[pymongo.MongoClient]) -> None:
    """Shrunk to less than an entry, each partition evicts all it holds
    (`numEvicted`); the counters are mongod's, field for field, and the
    budget and the partitions are mongod's on the same machine."""
    answers = []
    for client in servers:
        db = client[_DB]
        db.c.insert_one({"a": 1})
        client.admin.command("setParameter", 1, internalQueryStatsRateLimit=-1)
        status = client.admin.command("serverStatus")["metrics"]["queryStats"]
        fields = {k: type(v).__name__ for k, v in status.items()}
        sized = (status["maxSizeBytes"], status["numPartitions"])
        for n in range(5):
            list(db.c.find({f"f{n}": 1}))
        before = client.admin.command("serverStatus")["metrics"]["queryStats"]
        client.admin.command("setParameter", 1, internalQueryStatsCacheSize="1KB")
        after = client.admin.command("serverStatus")["metrics"]["queryStats"]
        answers.append(
            (
                fields,
                sized,
                before["numEntries"] >= 5,
                after["numEvicted"] - before["numEvicted"] >= 5,
                after["maxSizeBytes"],
                _ours(client),
            )
        )
    same(answers[0], answers[1])


_EXPLAINS = [
    SON([("aggregate", "c"), ("pipeline", [{"$match": {"e1": 1}}]), ("explain", True)]),
    SON(
        [
            ("aggregate", "c"),
            ("pipeline", [{"$match": {"e2": 1}}]),
            ("explain", True),
            ("cursor", {"batchSize": 5}),
        ]
    ),
    SON(
        [
            ("aggregate", "c"),
            ("pipeline", [{"$match": {"e3": 1}}]),
            ("explain", False),
            ("cursor", {}),
        ]
    ),
    SON(
        [
            ("explain", {"aggregate": "c", "pipeline": [{"$match": {"e4": 1}}], "cursor": {}}),
            ("comment", "outer"),
            ("maxTimeMS", 5000),
        ]
    ),
    SON(
        [
            ("explain", {"aggregate": "c", "pipeline": [{"$match": {"e5": 1}}], "cursor": {}}),
            ("verbosity", "executionStats"),
        ]
    ),
    SON(
        [
            (
                "explain",
                {"aggregate": "c", "pipeline": [{"$match": {"e6": 1}}], "comment": "inner"},
            ),
            ("verbosity", "queryPlanner"),
        ]
    ),
    SON([("explain", {"find": "c", "filter": {"e7": 1}})]),
    SON([("explain", {"count": "c", "query": {"e8": 1}})]),
    SON([("explain", {"distinct": "c", "key": "e9"})]),
]


def test_an_aggregate_that_explains_is_recorded(servers: list[pymongo.MongoClient]) -> None:
    """An `aggregate` that explains is recorded with its verbosity --
    `queryPlanner` for `{explain: true}`, the `explain` command's own
    (`allPlansExecution` by default) with its `comment` -- and a `cursor`
    only when it has one. An explained `find`, `count` or `distinct` is not
    recorded."""
    answers = []
    for client in servers:
        db = client[_DB]
        db.c.insert_one({"a": 1})
        client.admin.command("setParameter", 1, internalQueryStatsRateLimit=-1)
        for command in _EXPLAINS:
            db.command(command)
        answers.append(_ours(client))
    same(answers[0], answers[1])
