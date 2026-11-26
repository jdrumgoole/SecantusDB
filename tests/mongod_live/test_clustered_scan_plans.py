"""A find on a clustered collection is a scan of the table, as mongod plans it (RM-739).

mongod keys a clustered collection's records by `_id`, so a find that names
`_id` reads no index: an `_id` equality is an `EXPRESS_CLUSTERED_IXSCAN`, and
a range (an `$in` as the range of its first and last value) a
`CLUSTERED_IXSCAN` bounded by `minRecord` / `maxRecord`, in the direction of
a sort on `_id`. The scan is one more plan for the trial to try, against the
indexes a query's other predicates use (`collscanIsBounded`). Measured on
mongod 8.2.11 (2026-11-26): the plan, the plans it beat, and what each stage
counted -- the keys and documents examined, and the works of a plan that was
not contested.
"""

from __future__ import annotations

import random
import uuid
from typing import Any

import pytest

from tests.mongod_live.conftest import same

_CLUSTERED = {"key": {"_id": 1}, "unique": True, "name": "ci"}
_STAGE_KEYS = {
    "stage",
    "filter",
    "direction",
    "minRecord",
    "maxRecord",
    "limitAmount",
    "skipAmount",
    "sortPattern",
    "indexName",
    "indexBounds",
    "transformBy",
}
_COUNTERS = ("nReturned", "keysExamined", "docsExamined")


def _tree(stage: dict) -> dict:
    out = {k: v for k, v in stage.items() if k in _STAGE_KEYS}
    if "inputStage" in stage:
        out["inputStage"] = _tree(stage["inputStage"])
    if "inputStages" in stage:
        out["inputStages"] = [_tree(s) for s in stage["inputStages"]]
    return out


def _counted(stage: dict, contested: bool) -> dict:
    out = {k: stage[k] for k in _COUNTERS if k in stage}
    # A contested winner's works carry the trial's, which no comparison here
    # covers (`test_plan_trial.py`).
    if not contested:
        out.update({k: stage[k] for k in ("works", "advanced") if k in stage})
    if "inputStage" in stage:
        out["inputStage"] = _counted(stage["inputStage"], contested)
    if "inputStages" in stage:
        out["inputStages"] = [_counted(s, contested) for s in stage["inputStages"]]
    return out


def _seed(client: Any) -> Any:
    db = client[f"cs_{uuid.uuid4().hex[:12]}"]
    db.create_collection("c", clusteredIndex=_CLUSTERED)
    ids = list(range(1, 201))
    random.Random(7).shuffle(ids)
    db.c.insert_many([{"_id": i, "x": i % 7, "y": i % 50} for i in ids])
    db.create_collection("s", clusteredIndex=_CLUSTERED)
    db.s.insert_many([{"_id": f"k{i:03}"} for i in range(1, 6)] + [{"_id": i} for i in (1, 2, 3)])
    db.c.create_index("x")
    db.c.create_index([("y", 1), ("x", 1)])
    return db


# (collection, command fields, whether mongod's trial has several plans)
_FINDS: list[tuple[str, dict, bool]] = [
    ("c", {"filter": {"_id": 3}}, False),
    ("c", {"filter": {"_id": {"$eq": 3}}, "limit": 1}, False),
    ("c", {"filter": {"_id": 3}, "projection": {"x": 1}}, False),
    ("c", {"filter": {"_id": 3}, "batchSize": 1}, False),
    ("c", {"filter": {"_id": 3}, "skip": 1}, False),
    ("c", {"filter": {"_id": 3}, "collation": {"locale": "en"}}, False),
    ("c", {"filter": {"_id": 3}, "hint": "ci"}, False),
    ("c", {"filter": {"_id": 3}, "hint": {"$natural": 1}}, False),
    ("c", {"filter": {"_id": 3}, "showRecordId": True}, False),
    ("c", {"filter": {"_id": 999}}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}}, False),
    ("c", {"filter": {"_id": {"$gt": 1000}}}, False),
    ("c", {"filter": {"_id": {"$gte": 190, "$lte": 200}}}, False),
    ("c", {"filter": {"_id": {"$gt": 5, "$lt": 5}}}, False),
    ("c", {"filter": {"_id": {"$lt": 4}}}, False),
    ("c", {"filter": {"_id": {"$lte": 1}}}, False),
    ("c", {"filter": {"_id": {"$in": [4, 2, 4]}}}, False),
    ("c", {"filter": {"_id": {"$in": [4]}}}, False),
    ("c", {"filter": {"_id": {"$in": [7, "a", 2]}}}, False),
    ("c", {"filter": {"$and": [{"_id": {"$gt": 1}}, {"_id": {"$lt": 4}}]}}, False),
    ("c", {"filter": {"_id": {"$gt": 1}, "$and": [{"_id": {"$gt": 2}}]}}, False),
    ("c", {"filter": {"$expr": {"$eq": ["$_id", 2]}}}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "sort": {"_id": -1}}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "sort": {"_id": -1}, "limit": 2}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "sort": {"_id": 1}}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "skip": 2}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "projection": {"_id": 1}}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "batchSize": 3}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "collation": {"locale": "en"}}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "hint": "ci"}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "hint": {"_id": 1}}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "hint": {"$natural": -1}}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "hint": "x_1"}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "returnKey": True}, False),
    ("c", {"filter": {"_id": {"$gt": 190}}, "showRecordId": True}, False),
    ("c", {"filter": {}, "sort": {"_id": 1}}, False),
    ("c", {"filter": {}, "sort": {"_id": -1}, "limit": 2}, False),
    ("c", {"filter": {}, "hint": "ci"}, False),
    ("c", {"filter": {"_id": {"$ne": 3}}}, False),
    ("c", {"filter": {"_id": {"$regex": "^a"}}}, False),
    ("c", {"filter": {"_id": {"$exists": True}}}, False),
    ("c", {"filter": {"_id": {"$not": {"$gt": 2}}}}, False),
    ("c", {"filter": {"_id": None}}, False),
    ("c", {"filter": {"_id": [1]}}, False),
    ("c", {"filter": {"_id": {"$gt": 1, "$lt": "z"}}}, False),
    ("s", {"filter": {"_id": {"$gt": "k002"}}}, False),
    ("s", {"filter": {"_id": {"$lt": "k003"}}}, False),
    ("s", {"filter": {"_id": {"$gte": 2}}}, False),
    ("s", {"filter": {"_id": {"$gt": "k002"}}, "sort": {"_id": -1}}, False),
    # The scan against the indexes the other predicates use.
    ("c", {"filter": {"_id": {"$gt": 150}, "x": 1}}, True),
    ("c", {"filter": {"_id": {"$gt": 150}, "x": 1}, "limit": 5}, True),
    ("c", {"filter": {"_id": {"$gt": 10, "$lt": 30}, "x": 1}}, True),
    ("c", {"filter": {"_id": 100, "x": 2}}, True),
    ("c", {"filter": {"_id": 100, "x": 2, "y": 0}}, True),
    ("c", {"filter": {"_id": {"$gt": 1}, "y": 5}}, True),
    ("c", {"filter": {"_id": {"$lt": 10}, "y": 5}}, True),
    ("c", {"filter": {"_id": {"$gt": 50}}, "sort": {"x": 1}}, True),
    ("c", {"filter": {"_id": {"$gt": 5, "$lt": 9}}, "sort": {"y": 1}}, True),
    ("c", {"filter": {"_id": {"$gt": 5, "$lt": 9}}, "sort": {"y": 1, "x": 1}}, True),
    ("c", {"filter": {"x": 1}, "sort": {"_id": 1}}, True),
    ("c", {"filter": {"x": 1}, "sort": {"_id": -1}, "limit": 1}, True),
    ("c", {"filter": {"x": 1, "y": {"$gt": 40}}, "sort": {"_id": 1}}, True),
]


def _observe(client: Any) -> list:
    db = _seed(client)
    out = []
    for coll, fields, contested in _FINDS:
        try:
            e = db.command(
                {"explain": {"find": coll, **fields}, "verbosity": "executionStats"},
            )
        except Exception as err:  # noqa: BLE001 -- the refusal is the answer
            out.append({"find": fields, "refused": getattr(err, "code", str(err))})
            continue
        plan = e["queryPlanner"]
        stats = e["executionStats"]
        out.append(
            {
                "find": fields,
                "winningPlan": _tree(plan["winningPlan"]),
                "rejected": [_tree(r) for r in plan["rejectedPlans"]],
                "nReturned": stats["nReturned"],
                "totalKeysExamined": stats["totalKeysExamined"],
                "totalDocsExamined": stats["totalDocsExamined"],
                "stages": _counted(stats["executionStages"], contested),
            }
        )
    return out


@pytest.mark.mongod_live
def test_a_find_on_a_clustered_collection_is_planned_as_mongod_plans_it(
    rust_mongo: Any, mongod: Any
) -> None:
    ours, theirs = _observe(rust_mongo), _observe(mongod)
    for a, b in zip(ours, theirs, strict=True):
        same(a, b)


@pytest.mark.mongod_live
def test_the_documents_of_a_clustered_find_come_back_in_id_order(
    rust_mongo: Any, mongod: Any
) -> None:
    def read(client: Any) -> list:
        db = _seed(client)
        return [
            [d["_id"] for d in db.c.find({"_id": {"$gt": 100}}).limit(3)],
            [d["_id"] for d in db.c.find({"_id": {"$gt": 190}}).sort("_id", -1)],
            [d["_id"] for d in db.c.find({"_id": {"$in": [30, 3, 12]}})],
            [d["_id"] for d in db.c.find({"x": 3}).hint("x_1").limit(3)],
            [d["_id"] for d in db.c.find({"_id": {"$gt": 150}, "x": 1})],
            [d["_id"] for d in db.s.find({"_id": {"$lt": "k004"}})],
        ]

    same(read(rust_mongo), read(mongod))
