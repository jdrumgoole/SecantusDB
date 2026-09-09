"""What an upsert seeds into the document it inserts.

When an upsert finds no match, mongod builds the new document from the QUERY,
and it reads more than bare equality: `{a: {$eq: 1}}`, a one-element `$in` or
`$all`, every `$and` branch, and a *lone* `$or` branch all imply an equality and
are seeded. Both servers seeded only bare equality, so five query forms lost
their fields entirely — a silently wrong INSERT, since the document mongod would
have written is missing a field.

Measured against mongod 8.2.11 on 2026-09-08 over 20 queries × 6 updates
(120 cells): **24 divergent**, now 0.

**Field ORDER is deliberately not asserted.** mongod emits the seeded fields in
its own hash-table order — query `{a: 1, b: 2}` gives `b, a`, `{aa: 1, ab: 2}`
gives `aa, ab`, `{one, two, three}` gives `three, one, two` — which ignores the
query's order and is neither sorted nor reversed. That is an implementation
detail, not a contract, and CLAUDE.md records that it *changed* between 6.0.16
(sorted) and newer servers. These tests compare the seeded field/value pairs.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture(scope="module")
def coll(tmp_path_factory):
    home = tmp_path_factory.mktemp("upseed") / "wt"
    with SecantusDBServer(port=0, storage_path=str(home)) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        c = client["upseed"]["c"]
        try:
            yield c
        finally:
            client.close()


def seeded(coll, query, update=None):
    """The upserted document's fields, minus `_id` and the update's own."""
    coll.delete_many({})
    coll.update_one(query, update or {"$set": {"zz": 9}}, upsert=True)
    doc = coll.find_one({})
    doc.pop("_id")
    doc.pop("zz", None)
    return doc


SEEDS = [
    ({"a": 1}, {"a": 1}),
    ({"a": {"$eq": 1}}, {"a": 1}),
    ({"a": {"$in": [1]}}, {"a": 1}),
    ({"a": {"$all": [1]}}, {"a": 1}),
    ({"$and": [{"a": 1}, {"b": 2}]}, {"a": 1, "b": 2}),
    ({"$and": [{"a": {"$eq": 1}}]}, {"a": 1}),
    ({"$and": [{"$and": [{"a": 1}]}]}, {"a": 1}),
    ({"$or": [{"a": 1}]}, {"a": 1}),
    ({"a": {"$eq": 1}, "b": 2}, {"a": 1, "b": 2}),
    # The `$in` wins over a range operator on the same field.
    ({"a": {"$in": [1], "$gt": 0}}, {"a": 1}),
    # A dotted equality builds the nesting.
    ({"a.b": {"$eq": 1}}, {"a": {"b": 1}}),
    # A one-element `$in` whose element is itself an array seeds the array.
    ({"a": {"$in": [[1, 2]]}}, {"a": [1, 2]}),
]


@pytest.mark.parametrize("query,expected", SEEDS, ids=[str(q)[:34] for q, _ in SEEDS])
def test_query_forms_that_seed(coll, query, expected):
    assert seeded(coll, query) == expected


NO_SEED = [
    {"a": {"$in": [1, 2]}},  # two candidates imply nothing
    {"a": {"$in": []}},
    {"$or": [{"a": 1}, {"b": 2}]},  # two branches imply nothing
    {"$nor": [{"a": 1}]},
    {"a": {"$gt": 1}},
    {"a": {"$ne": 1}},
    {"a": {"$exists": True}},
    {"a": {"$type": "int"}},
    {"a": {"$not": {"$gt": 1}}},
    {"a": {"$elemMatch": {"b": 1}}},
]


@pytest.mark.parametrize("query", NO_SEED, ids=[str(q)[:34] for q in NO_SEED])
def test_query_forms_that_seed_nothing(coll, query):
    assert seeded(coll, query) == {}


MATCHED_TWICE = [
    ({"a": {"$all": [1, 2]}}, "a"),
    ({"$and": [{"a": 1}, {"a": 1}]}, "a"),
]


@pytest.mark.parametrize("query,path", MATCHED_TWICE, ids=[str(q)[:34] for q, _ in MATCHED_TWICE])
def test_two_clauses_implying_one_path_is_an_error(coll, query, path):
    """mongod refuses rather than picking one."""
    coll.delete_many({})
    with pytest.raises(OperationFailure) as excinfo:
        coll.update_one(query, {"$set": {"zz": 9}}, upsert=True)
    err = excinfo.value
    assert err.code == 54
    assert f"cannot infer query fields to set, path '{path}' is matched twice" in err.details.get(
        "errmsg", ""
    )


def test_the_update_still_wins_over_a_seeded_field(coll):
    assert seeded(coll, {"$and": [{"a": 1}, {"b": 2}]}, {"$set": {"a": 7}}) == {"a": 7, "b": 2}


def test_setoninsert_applies_alongside_the_seed(coll):
    got = seeded(coll, {"a": {"$eq": 1}}, {"$setOnInsert": {"s": 1}})
    assert got == {"a": 1, "s": 1}
