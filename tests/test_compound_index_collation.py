"""Compound-index collation: compound bare-equality and compound
prefix + trailing-operator pickers honour the per-index ``collation``
option, so a multi-field filter combined with a matching ``collation``
hits the compound index at IXSCAN.

Before this slice, the compound pickers
(``_pick_compound_eq_index`` / ``_pick_compound_range_index``)
skipped any collation-having index — a multi-field filter with a
``collation`` argument fell back to COLLSCAN even when a compound
index could have served it. The single-field collation path
(shipped in the previous slice) already worked.
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


CI_STRENGTH_2 = {"locale": "en", "strength": 2}


def _plan_stage(explain_doc: dict) -> str:
    return explain_doc.get("queryPlanner", {}).get("winningPlan", {}).get("stage", "")


def _input_stage(explain_doc: dict) -> str:
    return (
        explain_doc.get("queryPlanner", {})
        .get("winningPlan", {})
        .get("inputStage", {})
        .get("stage", "")
    )


def _picked_index(explain_doc: dict) -> str:
    return (
        explain_doc.get("queryPlanner", {})
        .get("winningPlan", {})
        .get("inputStage", {})
        .get("indexName", "")
    )


# ---------------------------------------------------------------------------
# Compound bare-equality + collation
# ---------------------------------------------------------------------------


def test_compound_bare_eq_with_matching_collation_ixscan(client) -> None:
    """{a: "x", b: "y"} against {a:1, b:1} compound index with
    matching strength-2 collation lights up at IXSCAN."""
    coll = client["coll_db"]["compound_eq"]
    coll.insert_many(
        [
            {"_id": 1, "a": "Alice", "b": "Boston"},
            {"_id": 2, "a": "BOB", "b": "boston"},
            {"_id": 3, "a": "alice", "b": "Brisbane"},
        ]
    )
    coll.create_index([("a", 1), ("b", 1)], collation=CI_STRENGTH_2)

    plan = coll.find({"a": "alice", "b": "BOSTON"}, collation=CI_STRENGTH_2).explain()
    assert _plan_stage(plan) == "FETCH"
    assert _input_stage(plan) == "IXSCAN"

    docs = list(coll.find({"a": "alice", "b": "BOSTON"}, collation=CI_STRENGTH_2))
    assert [d["_id"] for d in docs] == [1]


def test_compound_prefix_eq_with_collation_ixscan(client) -> None:
    """{a: "x"} (leading-prefix only) against a {a:1, b:1} compound
    collation index — covers via prefix scan."""
    coll = client["coll_db"]["compound_prefix"]
    coll.insert_many(
        [
            {"_id": 1, "a": "Alice", "b": "Boston"},
            {"_id": 2, "a": "ALICE", "b": "Brisbane"},
            {"_id": 3, "a": "BOB", "b": "Boston"},
        ]
    )
    coll.create_index([("a", 1), ("b", 1)], collation=CI_STRENGTH_2)

    plan = coll.find({"a": "alice"}, collation=CI_STRENGTH_2).explain()
    assert _input_stage(plan) == "IXSCAN"

    docs = sorted(coll.find({"a": "alice"}, collation=CI_STRENGTH_2), key=lambda d: d["_id"])
    assert [d["_id"] for d in docs] == [1, 2]


def test_compound_eq_collation_mismatch_collscan(client) -> None:
    """Query with no collation against a compound collation index → COLLSCAN."""
    coll = client["coll_db"]["compound_mismatch"]
    coll.insert_many([{"_id": i, "a": "Alice", "b": "Boston"} for i in [1, 2]])
    coll.create_index([("a", 1), ("b", 1)], collation=CI_STRENGTH_2)

    plan = coll.find({"a": "Alice", "b": "Boston"}).explain()  # no collation
    assert _plan_stage(plan) == "COLLSCAN"


def test_compound_eq_no_collation_index_picked_for_no_collation_query(client) -> None:
    """Two compound indexes on the same fields, one with collation
    and one without — a no-collation query picks the no-collation index."""
    coll = client["coll_db"]["compound_two_indexes"]
    coll.insert_many([{"_id": 1, "a": "Alice", "b": "Boston"}])
    coll.create_index([("a", 1), ("b", 1)], name="codepoint")
    coll.create_index([("a", 1), ("b", 1)], name="ci", collation=CI_STRENGTH_2)

    plan = coll.find({"a": "Alice", "b": "Boston"}).explain()
    assert _input_stage(plan) == "IXSCAN"
    assert _picked_index(plan) == "codepoint"

    plan = coll.find({"a": "alice", "b": "boston"}, collation=CI_STRENGTH_2).explain()
    assert _input_stage(plan) == "IXSCAN"
    assert _picked_index(plan) == "ci"


# ---------------------------------------------------------------------------
# Compound prefix + trailing operator + collation
# ---------------------------------------------------------------------------


def test_compound_range_trailing_operator_with_collation_ixscan(client) -> None:
    """{a: "X", b: {$gt: "k"}} against {a:1, b:1} collation index."""
    coll = client["coll_db"]["compound_range"]
    coll.insert_many(
        [
            {"_id": 1, "a": "Alice", "b": "Apple"},
            {"_id": 2, "a": "Alice", "b": "MANGO"},
            {"_id": 3, "a": "Alice", "b": "pear"},
            {"_id": 4, "a": "BOB", "b": "ZUCCHINI"},
        ]
    )
    coll.create_index([("a", 1), ("b", 1)], collation=CI_STRENGTH_2)

    plan = coll.find({"a": "alice", "b": {"$gt": "k"}}, collation=CI_STRENGTH_2).explain()
    assert _input_stage(plan) == "IXSCAN"

    docs = sorted(
        coll.find({"a": "alice", "b": {"$gt": "k"}}, collation=CI_STRENGTH_2),
        key=lambda d: d["_id"],
    )
    # Under strength-2: "MANGO" > "k", "pear" > "k", "Apple" < "k".
    assert [d["_id"] for d in docs] == [2, 3]


def test_compound_range_trailing_in_with_collation(client) -> None:
    """Trailing operator can be ``$in``."""
    coll = client["coll_db"]["compound_in"]
    coll.insert_many(
        [
            {"_id": 1, "a": "Alice", "b": "Apple"},
            {"_id": 2, "a": "Alice", "b": "MANGO"},
            {"_id": 3, "a": "Alice", "b": "pear"},
            {"_id": 4, "a": "BOB", "b": "MANGO"},
        ]
    )
    coll.create_index([("a", 1), ("b", 1)], collation=CI_STRENGTH_2)

    plan = coll.find(
        {"a": "alice", "b": {"$in": ["mango", "PEAR"]}}, collation=CI_STRENGTH_2
    ).explain()
    assert _input_stage(plan) == "IXSCAN"

    docs = sorted(
        coll.find({"a": "alice", "b": {"$in": ["mango", "PEAR"]}}, collation=CI_STRENGTH_2),
        key=lambda d: d["_id"],
    )
    assert [d["_id"] for d in docs] == [2, 3]


def test_compound_range_mismatch_falls_back_to_collscan(client) -> None:
    """Trailing-operator query with no collation against a
    collation-having compound index → COLLSCAN."""
    coll = client["coll_db"]["compound_range_mismatch"]
    coll.insert_many([{"_id": i, "a": "Alice", "b": chr(64 + i)} for i in [1, 2, 3]])
    coll.create_index([("a", 1), ("b", 1)], collation=CI_STRENGTH_2)

    plan = coll.find({"a": "Alice", "b": {"$gt": "A"}}).explain()
    assert _plan_stage(plan) == "COLLSCAN"


# ---------------------------------------------------------------------------
# Update path uses the same picker
# ---------------------------------------------------------------------------


def test_compound_update_uses_collation_index(client) -> None:
    """update_one with a multi-field filter + collation routes through
    the compound collation index."""
    coll = client["coll_db"]["compound_update"]
    coll.insert_one({"_id": 1, "a": "Alice", "b": "Boston", "v": 1})
    coll.create_index([("a", 1), ("b", 1)], collation=CI_STRENGTH_2)

    result = coll.update_one(
        {"a": "alice", "b": "BOSTON"},
        {"$set": {"v": 2}},
        collation=CI_STRENGTH_2,
    )
    assert result.modified_count == 1
    assert coll.find_one({"_id": 1})["v"] == 2


def test_compound_unique_with_collation_enforced(client) -> None:
    """A unique compound index with a collation rejects a second insert
    whose values collide under the collation."""
    coll = client["coll_db"]["compound_unique"]
    coll.create_index([("a", 1), ("b", 1)], unique=True, collation=CI_STRENGTH_2)
    coll.insert_one({"_id": 1, "a": "Alice", "b": "Boston"})

    from pymongo.errors import DuplicateKeyError

    with pytest.raises(DuplicateKeyError):
        coll.insert_one({"_id": 2, "a": "ALICE", "b": "BOSTON"})


def test_compound_numericordering_collation_falls_back(client) -> None:
    """numericOrdering still has no byte-sortable encoding, so a
    compound index with that collation parses to None at the gate
    and a numericOrdering query falls back to COLLSCAN."""
    coll = client["coll_db"]["compound_num"]
    coll.insert_many(
        [
            {"_id": 1, "a": "x", "b": "a2"},
            {"_id": 2, "a": "x", "b": "a10"},
        ]
    )
    coll.create_index(
        [("a", 1), ("b", 1)],
        collation={"locale": "en", "numericOrdering": True},
    )

    plan = coll.find(
        {"a": "x", "b": "a10"},
        collation={"locale": "en", "numericOrdering": True},
    ).explain()
    assert _plan_stage(plan) == "COLLSCAN"
    # Results still correct via matches().
    docs = list(
        coll.find(
            {"a": "x", "b": "a10"},
            collation={"locale": "en", "numericOrdering": True},
        )
    )
    assert [d["_id"] for d in docs] == [2]
