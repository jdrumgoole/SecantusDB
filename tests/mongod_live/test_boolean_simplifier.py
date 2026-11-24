"""mongod's Boolean expression simplifier over a `find`'s filter (RM-774).

mongod 8.2 reads the optimised filter as a tree of `$and` / `$or` over
leaf predicates, rewrites it through its disjunctive normal form
(Quine-McCluskey over bitset terms) and keeps the result when it is
smaller (`src/mongo/db/query/boolean_simplification/`). The rules are in
`crates/secantus-commands/src/explain/simplify.rs`; this compares the
filters that exercised each of them, and a seeded run of random trees,
with a live mongod through `explain`'s `parsedQuery`.
"""

from __future__ import annotations

import random
from typing import Any

import pytest

from tests.mongod_live.conftest import same

_NE1 = {"$ne": 1}

CASES: list[tuple[str, dict]] = [
    ("absorption", {"$or": [{"a": 1, "b": 1}, {"a": 1}]}),
    ("absorbed by a conjunct", {"$and": [{"a": 1}, {"$or": [{"a": 1}, {"b": 2}]}]}),
    ("a contradiction", {"a": 1, "$nor": [{"a": 1}]}),
    ("a contradiction of a regex", {"a": {"$regex": "x"}, "$nor": [{"a": {"$regex": "x"}}]}),
    ("a tautology", {"$or": [{"a": 1}, {"a": _NE1}]}),
    ("a tautology of a range", {"$or": [{"a": {"$gt": 1}}, {"a": {"$not": {"$gt": 1}}}]}),
    ("merging", {"$or": [{"a": 1, "b": 1}, {"a": 1, "b": _NE1}]}),
    (
        "the term a merge made does not absorb its neighbours",
        {"$or": [{"a": 1, "c": 1}, {"c": 1, "d": 1}, {"c": 1, "d": _NE1}]},
    ),
    (
        "a product that shrinks",
        {"$and": [{"$or": [{"a": 1}, {"b": 1}]}, {"$or": [{"a": 1}, {"c": 1}]}]},
    ),
    (
        "common predicates come out of an $and's $or",
        {"$and": [{"$or": [{"a": 1, "b": 1}, {"a": 1, "c": 1}]}, {"x": 1}]},
    ),
    ("no gain, left alone", {"$or": [{"a": 1, "b": 1}, {"a": 1, "c": 1}]}),
    (
        "a tie, left alone",
        {"$and": [{"$or": [{"a": 1}, {"b": 1}]}, {"$or": [{"a": _NE1}, {"c": 1}]}]},
    ),
    ("duplicates", {"$and": [{"a": 1}, {"a": 1}, {"b": 1}]}),
    ("duplicates in an $or", {"$or": [{"a": {"$gt": 1}}, {"d": {"$gt": 1}}, {"d": {"$gt": 1}}]}),
    ("equal values are one predicate", {"a": 1.0, "$or": [{"a": 1}, {"b": 1}]}),
    (
        "an $in's values are a set",
        {"$or": [{"a": {"$in": [1, 2]}, "b": 1}, {"a": {"$in": [2, 1]}}]},
    ),
    ("an $or of one value's equalities", {"$or": [{"a": 1}, {"a": 1}]}),
    ("an $in of one value beside another child", {"$or": [{"a": 1}, {"a": 1}, {"b": 1}]}),
    (
        "an $elemMatch is one predicate",
        {"$or": [{"a": {"$elemMatch": {"x": 1}}, "b": 1}, {"a": {"$elemMatch": {"x": 1}}}]},
    ),
    ("a $nor", {"$nor": [{"a": 1, "b": 1}, {"a": 1}]}),
    ("a $nor beside an $or", {"$or": [{"$nor": [{"a": 1, "b": 1}]}, {"$nor": [{"a": 1}]}]}),
    (
        "a contradiction beside an $or",
        {"$and": [{"$or": [{"b": 1}, {"d": 1}]}, {"a": 2}, {"a": {"$ne": 2}}]},
    ),
    (
        "a contained $or simplified in its $and's context",
        {"$and": [{"$or": [{"b": 1}, {"d": 1}, {"a": {"$ne": 2}}]}, {"a": 2}]},
    ),
    (
        "a merge that leaves an always-true term beside others",
        {
            "$or": [
                {"a": 2},
                {"b": 1},
                {"c": _NE1},
                {"d": {"$not": {"$gt": 1}}},
                {"$nor": [{"b": 1}, {"b": 1}]},
            ]
        },
    ),
    (
        "an $or that names a bit both ways is always true",
        {"$or": [{"a": 2}, {"b": 1}, {"c": _NE1}, {"b": _NE1}]},
    ),
    (
        "three $or's sharing a predicate",
        {
            "$and": [
                {"$or": [{"a": 1}, {"b": 1}]},
                {"$or": [{"a": 1}, {"c": 1}]},
                {"$or": [{"a": 1}, {"d": 1}]},
            ]
        },
    ),
]


def _parsed(client: Any, filt: dict) -> Any:
    db = client["rm774"]
    db.c.drop()
    db.c.insert_one({"a": 1})
    return db.command("explain", {"find": "c", "filter": filt})["queryPlanner"]["parsedQuery"]


@pytest.mark.parametrize(("why", "filt"), CASES, ids=[c[0] for c in CASES])
def test_a_filter_is_simplified_as_mongod_does(
    rust_mongo: Any, mongod: Any, why: str, filt: dict
) -> None:
    same(_parsed(rust_mongo, filt), _parsed(mongod, filt))


_LEAVES = [
    {"a": 1},
    {"b": 1},
    {"c": 1},
    {"a": 2},
    {"d": {"$gt": 1}},
    {"a": {"$ne": 1}},
    {"b": {"$ne": 1}},
    {"c": {"$ne": 1}},
    {"d": {"$not": {"$gt": 1}}},
]


def _tree(rng: random.Random, depth: int) -> dict:
    if depth == 0 or rng.random() < 0.3:
        return dict(rng.choice(_LEAVES))
    kind = rng.choice(["$and", "$or", "$or", "$and"])
    return {kind: [_tree(rng, depth - 1) for _ in range(rng.choice([2, 2, 3]))]}


def test_random_trees_are_simplified_as_mongod_does(rust_mongo: Any, mongod: Any) -> None:
    """Three hundred random trees of `$and` / `$or` over nine predicates."""
    rng = random.Random(774)
    for _ in range(300):
        filt = _tree(rng, rng.choice([2, 3, 3, 4]))
        try:
            same(_parsed(rust_mongo, filt), _parsed(mongod, filt))
        except AssertionError as err:
            raise AssertionError(f"filter {filt}: {err}") from None
