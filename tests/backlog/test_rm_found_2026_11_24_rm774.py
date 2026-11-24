"""Gap tests found working RM-774 (mongod's Boolean simplifier over a
`find`'s filter) on 2026-11-24, comparing with mongod 8.2.11.

`crates/secantus-commands/src/explain/simplify.rs` follows what the
simplifier measurably does (DNF per node, Quine-McCluskey, no Petrick, the
common predicates of an `$and` taken out, the result kept when smaller).
A seeded fuzz of random `$and` / `$or` trees over nine predicates agrees
with mongod on all but ~0.3 percent of them (and ~0.7 percent with nested
`$nor`); these are the smallest that do not.
"""

from __future__ import annotations

from typing import Any

import pytest

_A1_NOT = {"$ne": 1}


def _parsed(client: Any, filt: dict) -> Any:
    db = client["rm774gap"]
    db.c.drop()
    db.c.insert_one({"a": 1})
    return db.command("explain", {"find": "c", "filter": filt})["queryPlanner"]["parsedQuery"]


GAPS = [
    # mongod keeps the `b` of a merge that makes an always-true `$or` term
    # beside `a: 2` and `d`; this server's merge drops it.
    {
        "$and": [
            {"d": {"$gt": 1}},
            {"$and": [{"a": 2}, {"d": {"$gt": 1}}]},
            {
                "$or": [
                    {"b": _A1_NOT},
                    {"$and": [{"a": 1}, {"b": 1}]},
                    {"$and": [{"b": 1}, {"a": _A1_NOT}]},
                ]
            },
        ]
    },
    # mongod simplifies the branches of an `$or` under an `$and` in its
    # own context and keeps the `$and`'s other terms.
    {
        "$and": [
            {
                "$or": [
                    {"$and": [{"a": 2}, {"a": 2}, {"d": {"$gt": 1}}]},
                    {"$and": [{"d": {"$not": {"$gt": 1}}}, {"a": 2}]},
                ]
            },
            {"c": 1},
            {"$or": [{"b": _A1_NOT}, {"$and": [{"c": 1}, {"d": {"$gt": 1}}]}]},
        ]
    },
    # Where mongod's merges leave `a: 1` beside `a: {$in: [1, 2]}`.
    {
        "$or": [
            {
                "$and": [
                    {
                        "$or": [
                            {"$and": [{"a": 2}, {"a": 2}]},
                            {"$or": [{"a": 2}, {"a": 1}, {"a": _A1_NOT}]},
                            {"$and": [{"a": 1}, {"a": 1}]},
                        ]
                    },
                    {"b": _A1_NOT},
                ]
            },
            {"a": 2},
        ]
    },
]


@pytest.mark.backlog("RM-778")
@pytest.mark.parametrize("filt", GAPS, ids=["merge-keeps-b", "or-in-context", "in-beside-eq"])
def test_a_random_tree_is_simplified_as_mongod_does(
    rust_mongo: Any, mongod: Any, filt: dict
) -> None:
    assert _parsed(rust_mongo, filt) == _parsed(mongod, filt)
