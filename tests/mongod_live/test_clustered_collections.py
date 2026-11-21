"""A clustered collection, the Rust server against mongod 8.2.11.

Phase 5 of `tasks/mongod-storage-model-plan.md` keys a clustered
collection's table by the KeyString of `_id`, which is its RecordId. What a
client can see of that is compared here: the order of a scan, `$recordId`,
the sizes `collStats` and `listIndexes` report, and `spilledBytes` of a sort
that spills `showRecordId` documents (a spilled member holds its RecordId,
whose length differs by kind: an int `_id` of two key bytes, a string, an
ObjectId). Measured on 8.2.11, 2026-11-21.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest
from bson import ObjectId

from tests.mongod_live.conftest import same

_CLUSTERED = {"key": {"_id": 1}, "unique": True, "name": "c"}

_IDS = {
    "int": lambda i: i,
    "wide_int": lambda i: i * 100000,
    "string": lambda i: "k" + "z" * (i % 7) + str(i),
    "object_id": lambda i: ObjectId(f"{i:024x}"),
    "document": lambda i: {"a": i},
}


def _fresh(client: Any) -> Any:
    return client[f"cl_{uuid.uuid4().hex[:12]}"]


def _spilling(client: Any, db: Any, fill: Any) -> dict:
    """A sort of `showRecordId` documents under a 2000-byte budget."""
    client.admin.command("setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=2000)
    try:
        db.create_collection("c", clusteredIndex=_CLUSTERED)
        db.c.insert_many([{"_id": fill(i), "x": 100 - i} for i in range(100)])
        plan = db.command(
            {
                "explain": {
                    "find": "c",
                    "filter": {},
                    "sort": {"x": 1},
                    "showRecordId": True,
                    "allowDiskUse": True,
                },
                "verbosity": "executionStats",
            }
        )
    finally:
        client.admin.command(
            "setParameter", 1, internalQueryMaxBlockingSortMemoryUsageBytes=104857600
        )
    stage = plan["executionStats"]["executionStages"]
    while stage.get("stage") != "SORT":
        stage = stage["inputStage"]
    return {k: stage.get(k) for k in ("type", "spills", "spilledRecords", "spilledBytes")}


@pytest.mark.mongod_live
@pytest.mark.parametrize("kind", sorted(_IDS))
def test_a_spilled_sort_reports_the_record_id_of_a_clustered_row(
    rust_mongo: Any, mongod: Any, kind: str
) -> None:
    answers = [_spilling(client, _fresh(client), _IDS[kind]) for client in (rust_mongo, mongod)]
    assert answers[1]["spills"] > 0, answers
    same(*answers)


@pytest.mark.mongod_live
def test_a_scan_a_record_id_and_the_stats_of_a_clustered_collection(
    rust_mongo: Any, mongod: Any
) -> None:
    def observe(client: Any) -> dict:
        db = _fresh(client)
        db.create_collection("c", clusteredIndex=_CLUSTERED)
        ids = [3, "b", 1.5, ObjectId("65f000000000000000000001"), {"a": 1}, 10**12]
        db.c.insert_many([{"_id": i, "x": n} for n, i in enumerate(ids)])
        db.c.create_index("x")
        stats = db.command("collStats", "c")
        listed = [
            {k: v for k, v in ix.items() if k != "v"}
            for ix in db.command("listIndexes", "c")["cursor"]["firstBatch"]
        ]
        return {
            "order": [d["_id"] for d in db.c.find()],
            "backward": [d["_id"] for d in db.c.find().sort([("$natural", -1)])],
            "record_ids": [(d["_id"], d["$recordId"]) for d in db.c.find({}, show_record_id=True)],
            "by_range": [d["_id"] for d in db.c.find({"_id": {"$gt": 1, "$lt": 5}})],
            "by_index": [d["_id"] for d in db.c.find({"x": {"$gte": 3}}).hint("x_1")],
            "nindexes": stats["nindexes"],
            "index_names": sorted(stats["indexSizes"]),
            "listed": listed,
        }

    same(observe(rust_mongo), observe(mongod))


@pytest.mark.mongod_live
def test_a_duplicate_id_of_a_clustered_collection_is_the_same_error(
    rust_mongo: Any, mongod: Any
) -> None:
    def observe(client: Any) -> dict:
        db = _fresh(client)
        db.create_collection("c", clusteredIndex=_CLUSTERED)
        db.c.insert_one({"_id": 3})
        try:
            db.c.insert_one({"_id": 3, "v": 1})
        except Exception as e:  # noqa: BLE001
            return {k: v for k, v in e.details.items() if k != "errmsg"} | {
                "msg": e.details["errmsg"].replace(db.name, "DB")
            }
        return {}

    same(observe(rust_mongo), observe(mongod))
