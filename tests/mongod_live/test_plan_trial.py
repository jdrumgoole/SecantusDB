"""mongod's trial of candidate plans, as `explain` reports it (RM-154).

The plan cache keeps what the trial found: each candidate's per-stage work
accounting (`works`, `advanced`, `needTime`, `isEOF`, an index scan's
`seeks`), its score with mongod's tie-breaking bonuses, and the candidates
mongod enumerates. These cases pin the shapes whose trial the plan cache
work found off (2026-10-27, mongod 8.2.11): an index intersection over a
multikey index, a sort an index cannot return by exploding its points
behind a range, a scan whose first interval starts exclusive, a tie broken
by the documents examined and the index prefix, and a limit above 101.
`explain` with `allPlansExecution` lists the plans tried; the winner's own
run (`executionStages`) is not compared, nor timings and yields.
"""

from __future__ import annotations

from typing import Any

from tests.mongod_live.conftest import same

DB = "live_plan_trial"
#: Per run: timings, and the yields a plan made, which mongod makes when
#: 10 ms have passed (a cold first query yields where a warm one does not).
VOLATILE = {
    "saveState",
    "restoreState",
    "executionTimeMillisEstimate",
    "executionTimeMillis",
    "optimizationTimeMillis",
    "queryShapeHash",
    "$clusterTime",
    "operationTime",
}


def strip(x: Any) -> Any:
    if isinstance(x, dict):
        return {k: strip(v) for k, v in x.items() if k not in VOLATILE}
    if isinstance(x, list):
        return [strip(v) for v in x]
    return x


def _setup(client: Any) -> Any:
    client.drop_database(DB)
    col = client[DB].c
    col.insert_many(
        [{"_id": i, "a": i % 100, "b": (i // 20) % 100, "t": [i % 3, i % 5]} for i in range(2000)]
    )
    for ix in ("a", "b", [("a", 1), ("b", 1)], "t"):
        col.create_index(ix)
    return col


CASES = [
    # An intersection of `a_1` and the multikey `t_1`: one key each.
    {"filter": {"a": 4, "t": 2}},
    # A sort on `b` over `a_1_b_1` behind a range on `a`: sorted keys, no
    # merge of exploded scans.
    {"filter": {"b": {"$in": [1, 2, 3]}, "a": {"$gt": 10}}, "sort": {"b": -1}},
    # Scans that start at an exclusive bound.
    {"filter": {"a": {"$gt": 10}, "b": {"$in": [1, 2, 3]}}},
    # Tied plans: the docs-examined and index-prefix bonuses decide.
    {"filter": {"a": 1, "b": 1}},
    {"filter": {"a": {"$lt": 0}, "b": {"$lt": 0}}},
    # A limit above 101: the trial still stops at 101 documents.
    {"filter": {"a": {"$lt": 50}, "b": {"$lt": 50}}, "limit": 150},
    {"filter": {"a": {"$gte": 3}, "b": 2}, "sort": {"a": 1}},
    # A trial of more than 1000 calls into the plans, which yields.
    {"filter": {"a": {"$gte": 0}, "b": {"$gte": 0}, "z": 1}},
]


def _tried(client: Any) -> list:
    col = _setup(client)
    out = []
    for case in CASES:
        e = col.database.command("explain", {"find": "c", **case}, verbosity="allPlansExecution")
        qp = e["queryPlanner"]
        out.append(
            strip(
                {
                    "winningPlan": qp["winningPlan"],
                    "rejectedPlans": qp["rejectedPlans"],
                    "allPlansExecution": e["executionStats"]["allPlansExecution"],
                }
            )
        )
    return out


def test_the_trial_explain_lists(rust_mongo: Any, mongod: Any) -> None:
    same(_tried(rust_mongo), _tried(mongod))
