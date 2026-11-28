"""Found 2026-11-28 working RM-071: explain gaps beside its `$exists` slice."""

from __future__ import annotations

from typing import Any

import pytest


def _exists_explains(client: Any) -> Any:
    db = client["rm071x"]
    db.drop_collection("t")
    db.t.insert_many([{"a": 1, "c": 1}, {"a": 2}])
    out = []
    for value in [1, 0, "x", 0.0]:
        f = {"c": {"$exists": value}}
        ex = db.command("explain", {"find": "t", "filter": f}, verbosity="queryPlanner")
        qp = ex["queryPlanner"]
        rows = sorted(d["a"] for d in db.t.find(f))
        agg = db.command("aggregate", "t", pipeline=[{"$match": f}, {"$count": "n"}], explain=True)
        aqp = agg.get("queryPlanner") or agg["stages"][0]["$cursor"]["queryPlanner"]
        out.append(
            (
                value,
                qp["parsedQuery"],
                qp["winningPlan"].get("filter"),
                rows,
                aqp["parsedQuery"],
                str(aqp["winningPlan"]).count("'$exists': True"),
            )
        )
    return out


@pytest.mark.backlog("RM-845")
def test_explain_canonicalises_a_non_boolean_exists(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 prints `$exists: <truthy>` as `$exists: true` and
    `$exists: <falsy>` as `$not: {$exists: true}` in `parsedQuery` and the
    plan's `filter`, a string being truthy. This server echoes a number
    (`$exists: 1`), so an aggregate over it is not run in SBE, and
    canonicalises a string to `$not`. The rows returned already match."""
    assert _exists_explains(rust_mongo) == _exists_explains(mongod)
