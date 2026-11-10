"""A query-layer sort that spilled is profiled once per buffer mongod's
cursor stage loads for the first batch, and how many it loads follows what
the stages after it read: one past a window's answer, a window's upper end,
a partition and the next one's first document, the input an `$unwind` or a
`$match` gave its output from (RM-326; measured on mongod 8.2.11,
2026-11-10, over these batch sizes).
"""

from __future__ import annotations

from typing import Any

from tests.mongod_live.conftest import same

BUDGET = "internalQueryMaxBlockingSortMemoryUsageBytes"
BATCHES = (1, 31, 32, 33, 43, 44, 95, 96, 97, 101)

PIPELINES = {
    "running total": [
        {
            "$setWindowFields": {
                "sortBy": {"n": 1},
                "output": {"t": {"$sum": "$n", "window": {"documents": ["unbounded", "current"]}}},
            }
        }
    ],
    "two ahead": [
        {
            "$setWindowFields": {
                "sortBy": {"n": 1},
                "output": {"t": {"$sum": "$n", "window": {"documents": [-1, 2]}}},
            }
        }
    ],
    "partitioned total": [
        {
            "$setWindowFields": {
                "partitionBy": "$k",
                "sortBy": {"n": 1},
                "output": {"t": {"$sum": "$n"}},
            }
        }
    ],
    "partitioned rank": [
        {
            "$setWindowFields": {
                "partitionBy": "$k",
                "sortBy": {"n": 1},
                "output": {"r": {"$rank": {}}},
            }
        }
    ],
    "$unwind": [{"$sort": {"n": -1}}, {"$unwind": "$a"}, {"$project": {"_id": 1}}],
    "$match": [
        {"$sort": {"n": -1}},
        {"$addFields": {"z": {"$mod": ["$n", 3]}}},
        {"$match": {"z": 0}},
    ],
}


def _spills(client: Any) -> dict:
    client.drop_database("rm326live")
    db = client.rm326live
    db.c.insert_many(
        [
            {
                "_id": i,
                "k": i % 7,
                "s": "x" * 1000,
                "n": i,
                "a": [1, 2] if i % 3 == 0 else ([] if i % 3 == 1 else 5),
            }
            for i in range(300)
        ]
    )
    out = {}
    client.admin.command({"setParameter": 1, BUDGET: 50_000})
    db.command({"profile": 2})
    try:
        for name, pipeline in PIPELINES.items():
            for size in BATCHES:
                tag = f"rm326-{name}-{size}"
                db.c.aggregate(pipeline, comment=tag, batchSize=size).close()
                entry = db.system.profile.find_one({"command.comment": tag, "op": "command"})
                out[tag] = (entry or {}).get("sortSpills")
    finally:
        db.command({"profile": 0})
        client.admin.command({"setParameter": 1, BUDGET: 104_857_600})
        client.drop_database("rm326live")
    return out


def test_sort_counted_per_load_the_stages_read(rust_mongo: Any, mongod: Any) -> None:
    same(_spills(rust_mongo), _spills(mongod))
