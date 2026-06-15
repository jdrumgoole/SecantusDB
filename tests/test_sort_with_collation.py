"""Sort acceleration against a collation-having index.

Before this slice: any query carrying a ``collation`` argument that
needed sorting fell into the COLLSCAN+Python-sort path, even when
an index existed whose stored collation matched the query's and
whose key order would have given the requested sort for free.

This file pins the new behaviour: a sort on a single field or
multi-field spec hits the index when the query's ``collation``
matches the index's, in both ASC and DESC directions, single-field
and compound. Mismatched collations still fall back to a Python
sort, which is correct (the index's byte order doesn't reflect the
query's collation).
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield c
    finally:
        c.close()


CI = {"locale": "en", "strength": 2}


def _winning_plan(explain_doc: dict) -> dict:
    return explain_doc.get("queryPlanner", {}).get("winningPlan", {})


def _input_stage(explain_doc: dict) -> str:
    return _winning_plan(explain_doc).get("inputStage", {}).get("stage", "")


def _index_name(explain_doc: dict) -> str:
    return _winning_plan(explain_doc).get("inputStage", {}).get("indexName", "")


def _direction(explain_doc: dict) -> str:
    return _winning_plan(explain_doc).get("inputStage", {}).get("direction", "")


# ---------------------------------------------------------------------------
# Single-field sort + matching collation → IXSCAN
# ---------------------------------------------------------------------------


def test_single_field_sort_with_matching_collation_uses_index(client) -> None:
    """sort({name: 1}) + collation(strength=2) against a single-field
    collation index walks the index forward — explain reports IXSCAN,
    docs come back in collation-sorted order even when storage order
    differs."""
    coll = client["sortcoll_db"]["single_asc"]
    # Insert in scrambled order to prove the sort actually fires.
    coll.insert_many(
        [
            {"_id": 1, "name": "carol"},
            {"_id": 2, "name": "Alice"},
            {"_id": 3, "name": "BOB"},
        ]
    )
    coll.create_index("name", collation=CI)

    plan = coll.find().sort("name", 1).collation(CI).explain()
    assert _input_stage(plan) == "IXSCAN"
    assert _index_name(plan).startswith("name")
    assert _direction(plan) == "forward"

    docs = list(coll.find().sort("name", 1).collation(CI))
    # Under strength-2: Alice < BOB < carol (case-folded comparison).
    assert [d["_id"] for d in docs] == [2, 3, 1]


def test_single_field_sort_descending_with_matching_collation(client) -> None:
    """sort({name: -1}) against an ASC collation index walks backward.
    explain reports direction=backward; docs come back reversed."""
    coll = client["sortcoll_db"]["single_desc"]
    coll.insert_many(
        [
            {"_id": 1, "name": "carol"},
            {"_id": 2, "name": "Alice"},
            {"_id": 3, "name": "BOB"},
        ]
    )
    coll.create_index("name", collation=CI)

    plan = coll.find().sort("name", -1).collation(CI).explain()
    assert _input_stage(plan) == "IXSCAN"
    assert _direction(plan) == "backward"

    docs = list(coll.find().sort("name", -1).collation(CI))
    assert [d["_id"] for d in docs] == [1, 3, 2]


def test_single_field_sort_no_collation_query_against_collation_index_collscan(
    client,
) -> None:
    """No-collation sort against a collation-having index falls back to
    COLLSCAN+Python sort. The index's byte order is collation-normalised,
    so walking it would produce the wrong order for a codepoint-sort
    query."""
    coll = client["sortcoll_db"]["single_no_collation"]
    coll.insert_many(
        [
            {"_id": 1, "name": "carol"},
            {"_id": 2, "name": "Alice"},
            {"_id": 3, "name": "BOB"},
        ]
    )
    coll.create_index("name", collation=CI)

    plan = coll.find().sort("name", 1).explain()
    assert _winning_plan(plan).get("stage") == "COLLSCAN"

    docs = list(coll.find().sort("name", 1))
    # Codepoint order: "Alice" < "BOB" < "carol" (uppercase before lowercase).
    assert [d["_id"] for d in docs] == [2, 3, 1]


def test_single_field_sort_collation_mismatch_collscan(client) -> None:
    """Index with strength 2 collation, query with strength 3 → mismatch
    → COLLSCAN."""
    coll = client["sortcoll_db"]["single_mismatch"]
    coll.insert_many([{"_id": i, "name": s} for i, s in enumerate(["b", "A"])])
    coll.create_index("name", collation=CI)

    plan = coll.find().sort("name", 1).collation({"locale": "en", "strength": 3}).explain()
    assert _winning_plan(plan).get("stage") == "COLLSCAN"


# ---------------------------------------------------------------------------
# Filter on the sort field — in_sort_order avoids the Python re-sort
# ---------------------------------------------------------------------------


def test_filter_on_sort_field_with_collation_in_sort_order(client) -> None:
    """When the filter is on the sort field AND the collation matches,
    we still get the right order without Python re-sorting (the index
    walk is already in collation order)."""
    coll = client["sortcoll_db"]["filter_and_sort"]
    coll.insert_many(
        [
            {"_id": 1, "name": "ALICE"},
            {"_id": 2, "name": "alice"},
            {"_id": 3, "name": "BOB"},
            {"_id": 4, "name": "Alice"},
        ]
    )
    coll.create_index("name", collation=CI)

    plan = coll.find({"name": "alice"}).sort("name", 1).collation(CI).explain()
    assert _input_stage(plan) == "IXSCAN"

    docs = list(coll.find({"name": "alice"}).sort("name", 1).collation(CI))
    # All three "alice" variants match (strength-2), in any order from
    # the index — what matters is they were all found via the index.
    assert sorted(d["_id"] for d in docs) == [1, 2, 4]


# ---------------------------------------------------------------------------
# Multi-field sort matching a compound collation index
# ---------------------------------------------------------------------------


def test_multi_field_sort_matching_compound_collation_index(client) -> None:
    """sort([(a, 1), (b, -1)]) matches a {a:1, b:-1} compound collation
    index exactly when the collation matches — index walk."""
    coll = client["sortcoll_db"]["multi_match"]
    coll.insert_many(
        [
            {"_id": 1, "a": "Alpha", "b": "Zulu"},
            {"_id": 2, "a": "ALPHA", "b": "yankee"},
            {"_id": 3, "a": "BETA", "b": "Tango"},
        ]
    )
    coll.create_index([("a", 1), ("b", -1)], collation=CI)

    plan = coll.find().sort([("a", 1), ("b", -1)]).collation(CI).explain()
    assert _input_stage(plan) == "IXSCAN"
    assert _direction(plan) == "forward"

    docs = list(coll.find().sort([("a", 1), ("b", -1)]).collation(CI))
    # Under strength-2: a=alpha (1, 2) then a=beta (3). Within alpha,
    # b descending: Zulu > yankee → _id 1 first, _id 2 second.
    assert [d["_id"] for d in docs] == [1, 2, 3]


def test_multi_field_sort_compound_collation_inverse_walk(client) -> None:
    """sort([(a, -1), (b, 1)]) is the full inverse of {a:1, b:-1} —
    walks the index backward."""
    coll = client["sortcoll_db"]["multi_inverse"]
    coll.insert_many(
        [
            {"_id": 1, "a": "Alpha", "b": "Zulu"},
            {"_id": 2, "a": "BETA", "b": "Tango"},
            {"_id": 3, "a": "ALPHA", "b": "yankee"},
        ]
    )
    coll.create_index([("a", 1), ("b", -1)], collation=CI)

    plan = coll.find().sort([("a", -1), ("b", 1)]).collation(CI).explain()
    assert _input_stage(plan) == "IXSCAN"
    assert _direction(plan) == "backward"

    docs = list(coll.find().sort([("a", -1), ("b", 1)]).collation(CI))
    # Under strength-2: a desc → beta (2) first; then alpha — b asc →
    # yankee < Zulu → _id 3 then _id 1.
    assert [d["_id"] for d in docs] == [2, 3, 1]


def test_multi_field_sort_compound_collation_mismatch(client) -> None:
    """Compound collation index with strength=2 + no-collation sort →
    COLLSCAN. Index byte order is collation-normalised; the user wants
    codepoint order."""
    coll = client["sortcoll_db"]["multi_mismatch"]
    coll.insert_many([{"_id": 1, "a": "x", "b": "y"}])
    coll.create_index([("a", 1), ("b", 1)], collation=CI)

    plan = coll.find().sort([("a", 1), ("b", 1)]).explain()  # no collation
    assert _winning_plan(plan).get("stage") == "COLLSCAN"
