"""Gap tests for the Rust MongoDB server items found on 2026-11-08 while
working RM-331.

Each test asks mongod 8.2 live and asserts the Rust server gives the same
answer.
"""

from __future__ import annotations

from typing import Any

import pytest


def _hinted_fetch_filter(client: Any) -> Any:
    coll = client.rm331f.c
    coll.drop()
    coll.insert_one({"_id": 1, "s": "a", "u": 1, "t": 1})
    coll.create_index([("u", 1)], name="u_1")
    flt = {"s": {"$gte": "a", "$lte": "z"}, "t": 1}
    plan = client.rm331f.command(
        "explain", {"find": "c", "filter": flt, "hint": "u_1"}, verbosity="queryPlanner"
    )["queryPlanner"]["winningPlan"]
    return plan.get("filter")


@pytest.mark.backlog("RM-422")
def test_a_hint_that_bounds_nothing_keeps_the_canonical_filter_order(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.1 keeps the FETCH filter over a hinted index that no
    predicate bounds in the canonical order, `$eq` before `$lte` and
    `$gte` (`[t $eq, s $lte, s $gte]`); the Rust server sorts it by path
    (`[s $lte, s $gte, t $eq]`)."""
    assert _hinted_fetch_filter(rust_mongo) == _hinted_fetch_filter(mongod)
