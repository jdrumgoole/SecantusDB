"""How explain lists a pipeline `$match` the query does not take (RM-537).

mongod 8.2 optimises a pipeline `$match` (`DocumentSourceMatch`) with
`MatchExpression::optimize` but never `sortTree`, so explain lists its
clauses in the order they were written, reshaped only as the optimiser
reshapes them (`ListOfMatchExpression::getOptimizer`): an `$and` puts the
children of an `$and` among its children after its own (so do `$all` and an
`$expr` with twins, which parse to one), a node of one child is that child,
a `$nor` of one is its `$not`, duplicates stay, and an `$or`'s equalities on
one path become an `$in` listed first, with the equalities alone on their
path listed last. Only the query's `parsedQuery` is sorted.

Left out: an `$or` with two or more lone equalities, or two or more `$in`s
it builds. mongod gathers those in an unordered map and lists them in its
iteration order, which changes from one mongod process to the next
(`{$or: [{b: 1}, {a: 2}]}` listed `[b, a]` and then, after a restart,
`[a, b]`), so there is no order to compare. Measured on 8.2.11, 2026-11-14.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.mongod_live.conftest import same

_LIMIT = {"$limit": 3}


def _after_a_limit(f: dict) -> list[dict]:
    return [_LIMIT, {"$match": f}]


_GT = {"$gt": 1}

FILTERS: dict[str, dict] = {
    "fields": {"d": {"$gt": 0}, "c": 1},
    "one_field_two_operators": {"a": {"$gt": 0, "$lt": 5}, "b": 1},
    "same_path_two_operators": {"a": {"$exists": True, "$eq": 1}},
    "nested_and_goes_last": {"$and": [{"b": 1}, {"$and": [{"z": 1}, {"a": 1}]}], "c": 1},
    "and_of_documents": {"$and": [{"b": 1, "x": 1}, {"c": 1}]},
    "and_of_one_stays_in_place": {"c": 1, "$and": [{"b": 1}], "d": 1},
    "or_of_one_document": {"c": 1, "$or": [{"b": 1, "a": 2}], "d": 1},
    "or_of_one_clause": {"c": 1, "$or": [{"b": 1}], "d": 1},
    "duplicates_stay": {"$and": [{"b": 1}, {"a": 1}, {"b": 1}]},
    "ne_and_nin": {"b": {"$ne": 1}, "a": {"$nin": [2, 1]}},
    "nin_empty": {"c": 1, "a": {"$nin": []}},
    "nor": {"$nor": [{"b": 1}, {"a": 1}]},
    "nor_beside_a_field": {"z": 1, "$nor": [{"b": 1}, {"a": 1}]},
    "nor_of_one": {"c": 1, "$nor": [{"b": 1}], "d": 1},
    "nor_of_one_document": {"$nor": [{"b": 1, "a": 2}]},
    "expr_twin_ahead": {"z": 1, "$expr": {"$eq": ["$y", 2]}, "a": 1},
    "expr_twins_after": {"b": 1, "$expr": {"$and": [{"$eq": ["$y", 2]}, {"$eq": ["$a", 1]}]}},
    "two_exprs": {"$and": [{"$expr": {"$eq": ["$y", 2]}}, {"$expr": "$q"}]},
    "expr_true_drops": {"c": 1, "$expr": True, "d": 1},
    "exists_false": {"b": {"$exists": False}, "a": 1},
    "regex": {"b": {"$regex": "x", "$options": "i"}, "a": 1},
    "not": {"b": {"$not": {"$gt": 1}}, "a": 1},
    "not_of_two_operators": {"a": {"$not": {"$lt": 9, "$gt": 3}}, "b": 1},
    "in_of_one": {"b": 1, "a": {"$in": [4]}},
    "in_values_sorted": {"z": 1, "x": {"$in": [3, 1, 2, 1]}},
    "all_goes_last": {"t": {"$all": [3, 1]}, "a": 1},
    "all_of_elem_matches": {
        "t": {"$all": [{"$elemMatch": {"b": 1, "a": 2}}, {"$elemMatch": {"c": 1}}]},
        "d": 1,
    },
    "elem_match_value": {"t": {"$elemMatch": {"$lt": 9, "$gt": 3}}, "a": 1},
    "elem_match_document": {"t": {"$elemMatch": {"b": 1, "$and": [{"q": 1}], "a": {"$gt": 0}}}},
    "type_mod_size": {"t": {"$type": "int"}, "a": {"$mod": [2, 0]}, "b": {"$size": 2}},
    "comment_and_always_true_drop": {"b": 1, "$comment": "x", "$alwaysTrue": 1, "a": 1},
    "always_true_alone_stays": {"$alwaysTrue": 1},
    "and_of_always_true": {"$and": [{"$alwaysTrue": 1}]},
    "or_of_always_true": {"$or": [{"$alwaysTrue": 1}]},
    "expr_true_alone": {"$expr": True},
    "or_with_expr_true": {"$or": [{"b": 1}, {"$expr": True}]},
    "nor_with_expr_true": {"$nor": [{"$expr": True}]},
    "or_branches": {"$or": [{"y": _GT}, {"x": {"$exists": True}}]},
    "or_nested_or_goes_last": {"$or": [{"$or": [{"b": _GT}, {"a": _GT}]}, {"c": _GT}]},
    "or_of_documents": {"$or": [{"b": 1, "a": 1}, {"d": 1, "c": 1}]},
    "or_in_first": {"$or": [{"y": _GT}, {"x": 2}, {"x": 1}]},
    "or_in_then_lone_equality": {"$or": [{"z": _GT}, {"y": 1}, {"y": 2}, {"x": 5}]},
    "or_lone_equality_last": {"$or": [{"x": 1}, {"z": _GT}, {"z": _GT}]},
    "or_regex_joins_the_in": {"$or": [{"x": 2}, {"x": {"$regex": "^a"}}, {"y": _GT}]},
    "or_always_false_drops": {"$or": [{"a": {"$in": []}}, {"b": _GT}, {"c": 1}]},
    "or_in_beside_an_and_clause": {"$and": [{"$or": [{"x": 1}, {"x": 2}]}, {"b": 1}]},
}


def _listed(client: Any, pipeline: list[dict]) -> list[dict]:
    c = client["match_stage_order"]["c" + uuid.uuid4().hex[:10]]
    c.insert_many([{"_id": i, "x": i % 3, "y": i} for i in range(1, 8)])
    e = c.database.command("aggregate", c.name, pipeline=pipeline, explain=True)
    return [s for s in e.get("stages", [])[1:] if "$match" in s]


@pytest.mark.parametrize("name", list(FILTERS))
def test_a_match_after_the_cursor_is_listed_as_on_mongod(
    rust_mongo: Any, mongod: Any, name: str
) -> None:
    pipeline = _after_a_limit(FILTERS[name])
    same(_listed(rust_mongo, pipeline), _listed(mongod, pipeline))


def test_a_match_behind_a_group_is_listed_as_written(rust_mongo: Any, mongod: Any) -> None:
    pipeline = [
        {"$group": {"_id": "$x", "f": {"$first": "$y"}}},
        {"$match": {"f": 1, "_id": {"$exists": True}}},
    ]
    same(_listed(rust_mongo, pipeline), _listed(mongod, pipeline))
