"""Gap tests for what working RM-537 (a pipeline `$match` listed as
written) found on 2026-11-14, comparing with mongod 8.2.11.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

LEAVES = {
    # The first field is no operator, so this is the document form: mongod
    # parses `$and` inside it as a query.
    "elem_match_document_with_an_and": {
        "t": {"$elemMatch": {"b": 1, "$and": [{"q": 1}], "a": {"$gt": 0}}}
    },
    # A `$not` of two operators is the `$not` of their `$and`.
    "not_of_two_operators": {"a": {"$not": {"$lt": 9, "$gt": 3}}, "b": 1},
    # `$nin: []` excludes nothing, and beside another clause it goes.
    "nin_empty_beside_a_clause": {"c": 1, "a": {"$nin": []}},
}


def _parsed(client: Any) -> dict:
    c = client["rm_found_1114"]["c" + uuid.uuid4().hex[:10]]
    c.insert_one({"x": 1})

    def parsed(f: dict) -> Any:
        e = c.database.command("explain", {"find": c.name, "filter": f}, verbosity="queryPlanner")
        return e["queryPlanner"]["parsedQuery"]

    return {name: mongo_answer(lambda f=f: parsed(f)) for name, f in LEAVES.items()}


@pytest.mark.backlog("RM-660")
def test_parsed_query_parses_these_leaves_as_mongod(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 parses an `$elemMatch` document whose first field is a
    path as a query (`{$and: [b, q, a]}`), a `$not` of two operators as
    `{$nor: [{$and: [...]}]}`, and drops a `$nin: []` beside another
    clause. This server echoes the `$elemMatch` document as written, keeps
    `{$not: {$lt, $gt}}` whole, and keeps `{a: {$not: {$in: []}}}`."""
    want = _parsed(mongod)
    assert want["nin_empty_beside_a_clause"] == ("ok", {"c": {"$eq": 1}})
    assert _parsed(rust_mongo) == want
