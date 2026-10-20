"""RM-219: `dbCheck` on a one-member replica set, measured against mongod
8.2.11: the oplog command entries it writes for secondaries (per-batch MD5 of
the documents in `_id` order) and its `local.system.healthlog` records.
"""

from __future__ import annotations

import time
from typing import Any

import pytest
from pymongo.errors import PyMongoError

from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture


def _run(client: Any, cmd: dict) -> tuple[list, list]:
    """The command entries and health-log records one `dbCheck` leaves.
    mongod runs it in the background, so this waits for its stop entry."""
    oplog = client.local["oplog.rs"]
    start = oplog.find_one(sort=[("$natural", -1)])["ts"]
    skip = client.local["system.healthlog"].count_documents({})
    client.rm219d.command(cmd)
    deadline = time.monotonic() + 30
    while not oplog.find_one({"ts": {"$gt": start}, "op": "c", "o.type": "stop"}):
        assert time.monotonic() < deadline, "dbCheck did not finish"
        time.sleep(0.1)
    entries = [
        (
            e["ns"],
            {k: v for k, v in e["o"].items() if k not in ("batchId", "readTimestamp", "uuid")},
        )
        for e in oplog.find({"ts": {"$gt": start}, "op": "c"})
    ]
    records = [
        {k: v for k, v in r.items() if k not in ("_id", "timestamp", "collectionUUID")}
        for r in client.local["system.healthlog"].find().skip(skip)
    ]
    return entries, records


def _db_check(client: Any) -> list:
    out: list = []
    for cmd, n in [
        ({"dbCheck": "d"}, 5),
        ({"dbCheck": "d", "maxDocsPerBatch": 2}, 5),
        ({"dbCheck": "d", "maxDocsPerBatch": 2}, 4),
        ({"dbCheck": "d", "maxCount": 2}, 5),
        ({"dbCheck": "d", "maxCount": 7}, 5),
        ({"dbCheck": "d"}, 0),
        ({"dbCheck": "d", "validateMode": "dataConsistencyAndMissingIndexKeysCheck"}, 3),
        ({"dbCheck": "d", "minKey": 1, "maxKey": 3}, 5),
    ]:
        client.drop_database("rm219d")
        client.rm219d.create_collection("d")
        if n:
            client.rm219d.d.insert_many([{"_id": i, "v": "x" * i} for i in range(n)])
        out.append((cmd, n, _run(client, cmd)))
    client.rm219d.command("create", "v", viewOn="d", pipeline=[])
    for cmd in [
        {"dbCheck": "nope"},
        {"dbCheck": "v"},
        {"dbCheck": "d", "x": 1},
        {"dbCheck": "d", "validateMode": "x"},
        {"dbCheck": "d", "validateMode": "extraIndexKeysCheck"},
    ]:
        try:
            client.rm219d.command(cmd)
            out.append((cmd, "ok"))
        except PyMongoError as e:
            out.append(
                (cmd, getattr(e, "code", None), (getattr(e, "details", None) or {}).get("errmsg"))
            )
    out.append(
        client.local.command("listCollections", filter={"name": "system.healthlog"})["cursor"][
            "firstBatch"
        ][0]["options"]
    )
    client.drop_database("rm219d")
    return out


@pytest.mark.backlog_fixed("RM-219")
def test_db_check_one_collection_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """Batches split by `maxDocsPerBatch`, cut short by `maxCount`, each with
    the MD5 mongod computes; start and stop records in the health log."""
    assert _db_check(rust_mongo) == _db_check(mongod_rs)


def _whole_database(client: Any) -> tuple:
    client.drop_database("rm219d")
    for name in ["zz", "aa", "mm"]:
        client.rm219d[name].insert_one({"_id": 1, "n": name})
    client.rm219d.command("create", "v", viewOn="aa", pipeline=[{"$match": {}}])
    entries, records = _run(client, {"dbCheck": 1})
    client.drop_database("rm219d")
    # mongod walks its catalog in its own internal order, so the batches are
    # compared as a set.
    batches = sorted((ns, o["md5"]) for ns, o in entries if o.get("type") == "batch")
    return entries[0], entries[-1], batches, records


@pytest.mark.backlog_fixed("RM-219")
def test_db_check_whole_database_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """`dbCheck: 1` checks every collection of the database, `system.views`
    included, between a start and a stop entry."""
    assert _whole_database(rust_mongo) == _whole_database(mongod_rs)
