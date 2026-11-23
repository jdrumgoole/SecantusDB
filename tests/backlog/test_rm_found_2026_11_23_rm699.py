"""Gap test found working RM-699 on 2026-11-23, comparing with mongod
8.2.11: mongod runs its Boolean expression simplifier
(`internalQueryEnableBooleanExpressionsSimplifier`, on by default) over a
`find`'s filter, and the Rust MongoDB server has no simplifier at all."""

from __future__ import annotations

from typing import Any

import pytest


def _parsed(client: Any, filt: dict) -> dict:
    db = client["rm699"]
    db.c.insert_one({"a": 1})
    return db.command("explain", {"find": "c", "filter": filt})["queryPlanner"]["parsedQuery"]


@pytest.mark.backlog("RM-774")
@pytest.mark.parametrize(
    ("filt", "want"),
    [
        ({"$or": [{"a": 1, "b": 1}, {"a": 1}]}, {"a": {"$eq": 1}}),
        ({"a": 1, "$nor": [{"a": 1}]}, {"$alwaysFalse": 1}),
        ({"$and": [{"a": 1}, {"$or": [{"a": 1}, {"b": 2}]}]}, {"a": {"$eq": 1}}),
    ],
)
def test_a_find_filter_is_boolean_simplified(
    rust_mongo: Any, mongod_rs: Any, filt: dict, want: dict
) -> None:
    """mongod 8.2.11 absorbs `$or: [{a, b}, {a}]` to `a`, and folds a
    contradiction to `$alwaysFalse`; this server prints the filter as given."""
    assert _parsed(mongod_rs, filt) == want
    assert _parsed(rust_mongo, filt) == want
