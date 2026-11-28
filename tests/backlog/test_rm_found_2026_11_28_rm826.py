"""Divergences found while closing RM-826 (2026-11-28), each measured against
mongod 8.2.11 by counting the plans `explain` lists."""

from __future__ import annotations

from typing import Any

import pytest


def _rejected(client: Any, indexes: list, query: dict) -> int:
    client.drop_database("rm826_found")
    col = client.rm826_found.c
    col.insert_many(
        [
            {"_id": i, "a": i % 100, "b": (i // 20) % 100, "c": i % 7, "d": i % 11}
            for i in range(2000)
        ]
    )
    for keys in indexes:
        col.create_index(keys)
    reply = client.rm826_found.command(
        "explain", {"find": "c", "filter": query}, verbosity="queryPlanner"
    )
    return len(reply["queryPlanner"].get("rejectedPlans", []))


@pytest.mark.backlog("RM-851")
def test_two_ors_in_one_and_are_each_planned(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 plans `{$and: [{$or: [{a: 1}, {b: 2}]}, {$or: [{c: 3},
    {d: 4}]}]}` over `a_1`, `b_1`, `c_1` and `d_1` through either `$or`, one
    rejected plan; this server plans the first `$or` only."""
    q = {"$and": [{"$or": [{"a": 1}, {"b": 2}]}, {"$or": [{"c": 3}, {"d": 4}]}]}
    ix = ["a", "b", "c", "d"]
    assert _rejected(mongod, ix, q) == 1
    assert _rejected(rust_mongo, ix, q) == _rejected(mongod, ix, q)


@pytest.mark.backlog("RM-852")
def test_an_or_of_a_compound_branch_enumerates_mongods_combinations(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 lists seven rejected plans for `{$or: [{a: 3, b: 1},
    {b: 5}], c: {$lt: 3}}` over `a_1`, `b_1`, `a_1_c_1`, `b_1_c_1` and
    `c_1_a_1` (eight plans with the one winner, which reads `b_1` under a
    FETCH and `b_1_c_1`); this server lists ten, the product of its
    branches' indexes, and winners another."""
    q = {"$or": [{"a": 3, "b": 1}, {"b": 5}], "c": {"$lt": 3}}
    ix = ["a", "b", [("a", 1), ("c", 1)], [("b", 1), ("c", 1)], [("c", 1), ("a", 1)]]
    assert _rejected(mongod, ix, q) == 7
    assert _rejected(rust_mongo, ix, q) == _rejected(mongod, ix, q)
