"""A sort with a limit over its budget spills as mongod's top-k sorter does
(`TopKSorter`, RM-399): runs of what it holds, a cutoff that turns away
documents once enough better ones were kept, merged runs stopped at the
limit, and a `$limit` the sort takes past stages that pass each document on
(`extractLimitForPushdown`), in a pipeline and in a `find`. What it spills
depends on the order the plan hands the documents over, so the input is
read both in and out of the sort's order (measured on mongod 8.2.11,
2026-11-10: every case below matched).
"""

from __future__ import annotations

from typing import Any

from tests.mongod_live.conftest import same

BUDGET = "internalQueryMaxBlockingSortMemoryUsageBytes"
FIELDS = ("sortSpills", "sortSpilledRecords", "sortSpilledBytes", "sortTotalDataSizeBytes")
ADD = {"$addFields": {"z": 1}}


def _spills(client: Any) -> dict:
    client.drop_database("rm399live")
    db = client.rm399live
    out = {}
    client.admin.command({"setParameter": 1, BUDGET: 50_000})
    db.command({"profile": 2})
    try:
        for order, n in (("in order", lambda i: i), ("shuffled", lambda i: (i * 37) % 300)):
            db.c.drop()
            db.c.insert_many(
                [{"_id": i, "k": i % 7, "s": "x" * 1000, "n": n(i)} for i in range(300)]
            )
            for way in (1, -1):
                sort = {"$sort": {"n": way}}
                pipelines = {
                    f"limit {k} {where}": pipeline
                    for k in (30, 50, 100, 200)
                    for where, pipeline in (
                        ("after", [sort, {"$limit": k}, ADD]),
                        ("past $addFields", [sort, ADD, {"$limit": k}]),
                    )
                }
                pipelines["skip and limit"] = [sort, {"$skip": 10}, ADD, {"$limit": 40}]
                for name, pipeline in pipelines.items():
                    tag = f"rm399-{order}-{way}-{name}"
                    list(db.c.aggregate(pipeline, comment=tag))
                    entry = db.system.profile.find_one({"command.comment": tag}) or {}
                    out[tag] = {k: entry.get(k) for k in FIELDS}
                for k in (50, 200):
                    tag = f"rm399-{order}-{way}-find limit {k}"
                    list(db.c.find({}).sort("n", way).limit(k).comment(tag))
                    entry = db.system.profile.find_one({"command.comment": tag}) or {}
                    out[tag] = {f: entry.get(f) for f in FIELDS}
    finally:
        db.command({"profile": 0})
        client.admin.command({"setParameter": 1, BUDGET: 104_857_600})
        client.drop_database("rm399live")
    return out


def test_top_k_sort_spills_as_mongods(rust_mongo: Any, mongod: Any) -> None:
    same(_spills(rust_mongo), _spills(mongod))
