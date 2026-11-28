"""A gap found converting RM-610's explain test (RM-728, 2026-11-28):
`serverStatus` has no `metrics.query.multiPlanner`, the one place mongod
counts the work of an `explain` at `queryPlanner`.

An `explain` at `queryPlanner` reports no counts, but it still runs the
plan trial, and mongod's `serverStatus` counts it: over the contested find
with a limit (indexes on `a` and `b`, filter `{a: {$gte: 0}, b: {$gte: 0}}`,
limit 101) the trial ran two plans for 202 works, over 10,000 documents and
over 100,000 alike (mongod 8.2.11, 2026-11-28). That is a count a test can
compare in place of the time the explain took.
"""

from __future__ import annotations

from typing import Any

import pytest

_QUERY = {"a": {"$gte": 0}, "b": {"$gte": 0}}
_COUNTERS = ("classicCount", "classicNumPlans", "classicWorks")


def _planner(client: Any) -> dict[str, int]:
    multi = client.admin.command("serverStatus")["metrics"].get("query", {}).get("multiPlanner", {})
    return {k: multi.get(k, 0) for k in _COUNTERS} | {
        "hitResultsLimit": multi.get("stoppingCondition", {}).get("hitResultsLimit", 0)
    }


def _trial_work(client: Any) -> dict[int, dict[str, int]]:
    """What an `explain` at `queryPlanner` of the contested find adds to
    the multi-planner's counters, over 10,000 documents and 100,000."""
    db = client["rm_728_multiplanner"]
    out: dict[int, dict[str, int]] = {}
    try:
        for n in (10_000, 100_000):
            db.drop_collection("c")
            for start in range(0, n, 10_000):
                db.c.insert_many(
                    [{"_id": i, "a": i, "b": i % 7} for i in range(start, start + 10_000)]
                )
            db.c.create_index("a")
            db.c.create_index("b")
            before = _planner(client)
            db.command(
                "explain", {"find": "c", "filter": _QUERY, "limit": 101}, verbosity="queryPlanner"
            )
            after = _planner(client)
            out[n] = {k: after[k] - before[k] for k in (*_COUNTERS, "hitResultsLimit")}
    finally:
        client.drop_database("rm_728_multiplanner")
    return out


@pytest.mark.backlog("RM-856")
def test_an_explain_at_query_planner_counts_its_trial_in_server_status(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    theirs = _trial_work(mongod_rs)
    assert theirs[10_000] == theirs[100_000], theirs
    assert theirs[100_000]["classicWorks"] == 202, theirs
    assert _trial_work(rust_mongo) == theirs
