"""Gap tests for what working RM-532 (`$queryStats`' cache size, explains
and `transformIdentifiers`, 2026-11-14) found in the query stats key: a
`$natural` sort, and the namespaces a hybrid search reads.

Each test runs on servers of its own: what a store records outlives the
parameter that let it record, and would show in every later `$queryStats`
on a shared server.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import pymongo
import pytest
from bson.son import SON

from tests.backlog.conftest import rust_mongo_server


@pytest.fixture
def own_servers(tmp_path: Any) -> Iterator[tuple[Any, Any]]:
    from tests.mongod_reference import MONGOD, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod()
    reference = pymongo.MongoClient(uri, serverSelectionTimeoutMS=10_000)
    try:
        with rust_mongo_server(tmp_path / "store") as ours:
            yield ours, reference
    finally:
        reference.close()
        _stop_mongod(proc, dbpath)


def _keys(client: Any, command: SON) -> list[Any]:
    """The keys `command` is recorded under, less the client's metadata."""
    db = client.rm532f
    db.c.insert_many([{"x": i} for i in range(3)])
    client.admin.command("setParameter", 1, internalQueryStatsRateLimit=-1)
    reply = db.command(command)
    cursor = reply.get("cursor") or {}
    while cursor.get("id"):
        cursor = db.command("getMore", cursor["id"], collection="c")["cursor"]
    keys = []
    for e in client.admin.aggregate([{"$queryStats": {}}]):
        key = e["key"]
        if key["queryShape"]["cmdNs"].get("db") == "rm532f":
            key.pop("client", None)
            keys.append(key)
    return keys


@pytest.mark.backlog("RM-643")
def test_a_natural_sort_is_recorded_as_a_hint(own_servers: Any) -> None:
    """mongod 8.2.11 records a `find` sorted on `$natural` with no `sort`
    in its shape and the sort as its key's `hint` (`{$natural: -1}`). This
    server records the sort in the shape and no hint."""
    ours, theirs = own_servers
    command = SON([("find", "c"), ("filter", {}), ("sort", {"$natural": -1})])
    assert _keys(ours, command) == _keys(theirs, command)


@pytest.mark.backlog("RM-644")
def test_a_hybrid_search_names_the_collection_it_unions(own_servers: Any) -> None:
    """mongod 8.2.11 lists the `$unionWith` a `$rankFusion` desugars its
    second input pipeline into among the key's `otherNss` -- the
    aggregate's own collection. This server walks the pipeline as written,
    which names no other namespace, and prints no `otherNss`."""
    ours, theirs = own_servers
    command = SON(
        [
            ("aggregate", "c"),
            (
                "pipeline",
                [
                    {
                        "$rankFusion": {
                            "input": {
                                "pipelines": {
                                    "a": [{"$sort": {"x": 1}}],
                                    "b": [{"$sort": {"x": -1}}],
                                }
                            }
                        }
                    }
                ],
            ),
            ("cursor", {}),
        ]
    )
    assert _keys(ours, command) == _keys(theirs, command)
