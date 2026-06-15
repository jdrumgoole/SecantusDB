"""Per-index collation: indexes whose stored ``collation`` matches
the query's collation accelerate the lookup; mismatched indexes
fall through to COLLSCAN.

Before this slice: any query carrying a ``collation`` argument
fell through to COLLSCAN by design because index entries were
written in BSON codepoint order (the collation infrastructure
existed for ``matches()`` / ``count`` / ``distinct`` /
``findAndModify`` but not for the index-write or
index-lookup paths). This file pins the new behaviour: matching
collations light up at IXSCAN, mismatched stay COLLSCAN.
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


def _plan_stage(explain_doc: dict) -> str:
    """Top-level stage of the winning plan."""
    return explain_doc.get("queryPlanner", {}).get("winningPlan", {}).get("stage", "")


def _input_stage(explain_doc: dict) -> str:
    """The stage *wrapped* by the FETCH — IXSCAN means we hit an index."""
    return (
        explain_doc.get("queryPlanner", {})
        .get("winningPlan", {})
        .get("inputStage", {})
        .get("stage", "")
    )


# ---------------------------------------------------------------------------
# Routing: matching collation → IXSCAN; mismatched → COLLSCAN
# ---------------------------------------------------------------------------


def test_collation_index_lights_up_at_ixscan(client) -> None:
    """A query carrying a collation that matches an index's stored
    collation hits the index."""
    coll = client["coll_db"]["routing_match"]
    coll.insert_many([{"_id": i, "name": n} for i, n in enumerate(["Alice", "BOB", "carol"])])
    coll.create_index("name", collation={"locale": "en", "strength": 2})

    plan = coll.find({"name": "alice"}, collation={"locale": "en", "strength": 2}).explain()
    assert _plan_stage(plan) == "FETCH"
    assert _input_stage(plan) == "IXSCAN"


def test_collation_mismatch_falls_back_to_collscan(client) -> None:
    """Query has collation, index doesn't — picker correctly skips
    the index and the run falls back to COLLSCAN."""
    coll = client["coll_db"]["routing_mismatch"]
    coll.insert_many([{"_id": i, "name": n} for i, n in enumerate(["Alice", "BOB"])])
    coll.create_index("name")  # NO collation

    plan = coll.find({"name": "alice"}, collation={"locale": "en", "strength": 2}).explain()
    assert _plan_stage(plan) == "COLLSCAN"


def test_no_collation_query_against_collation_index_collscan(client) -> None:
    """The other direction: index has collation, query doesn't. They
    don't match, so the query falls back to COLLSCAN.

    Important because the index entries are stored under the
    collation-normalised bytes, so a raw-codepoint lookup against
    them would miss everything except docs that happen to already
    be in normalised form.
    """
    coll = client["coll_db"]["routing_no_query"]
    coll.insert_many([{"_id": i, "name": n} for i, n in enumerate(["Alice", "BOB"])])
    coll.create_index("name", collation={"locale": "en", "strength": 2})

    plan = coll.find({"name": "Alice"}).explain()  # no collation arg
    assert _plan_stage(plan) == "COLLSCAN"


# ---------------------------------------------------------------------------
# Correctness: results match what matches() would produce
# ---------------------------------------------------------------------------


def test_case_insensitive_equality_via_index(client) -> None:
    """Strength-2 collation: case-insensitive equality. Querying
    'alice' finds the doc stored as 'Alice' via the index."""
    coll = client["coll_db"]["case_insensitive_eq"]
    coll.insert_many(
        [
            {"_id": 1, "name": "Alice"},
            {"_id": 2, "name": "BOB"},
            {"_id": 3, "name": "carol"},
            {"_id": 4, "name": "DAVE"},
        ]
    )
    coll.create_index("name", collation={"locale": "en", "strength": 2})

    docs = list(coll.find({"name": "alice"}, collation={"locale": "en", "strength": 2}))
    assert [d["_id"] for d in docs] == [1]

    docs = list(coll.find({"name": "BOB"}, collation={"locale": "en", "strength": 2}))
    assert [d["_id"] for d in docs] == [2]


def test_accent_insensitive_equality_via_index(client) -> None:
    """Strength-1 collation strips accents AND folds case. Query
    'cafe' finds 'Café' via the index."""
    coll = client["coll_db"]["accent_insensitive_eq"]
    coll.insert_many(
        [
            {"_id": 1, "name": "Café"},
            {"_id": 2, "name": "naïve"},
            {"_id": 3, "name": "résumé"},
        ]
    )
    coll.create_index("name", collation={"locale": "en", "strength": 1})

    docs = list(coll.find({"name": "cafe"}, collation={"locale": "en", "strength": 1}))
    assert [d["_id"] for d in docs] == [1]

    docs = list(coll.find({"name": "RESUME"}, collation={"locale": "en", "strength": 1}))
    assert [d["_id"] for d in docs] == [3]


def test_range_query_via_collation_index(client) -> None:
    """Range operators ($gt / $gte / $lt / $lte) also route through
    the collation index — the encoded bytes sort by the collation's
    rules so a range scan gives collation-correct results."""
    coll = client["coll_db"]["range"]
    coll.insert_many(
        [
            {"_id": 1, "name": "Alpha"},
            {"_id": 2, "name": "bravo"},
            {"_id": 3, "name": "CHARLIE"},
            {"_id": 4, "name": "delta"},
        ]
    )
    coll.create_index("name", collation={"locale": "en", "strength": 2})

    # Case-insensitive range: "b" < name <= "d" should match bravo,
    # CHARLIE, but NOT delta (d == d, but range is exclusive on lt).
    # Actually <= 'd' includes anything starting with 'd' that equals
    # the bound under the collation; let me think... 'delta' > 'd'
    # under string compare so delta is excluded by name <= "d".
    docs = list(
        coll.find(
            {"name": {"$gt": "B", "$lte": "delta"}},
            collation={"locale": "en", "strength": 2},
        )
    )
    found = sorted(d["name"] for d in docs)
    # Under strength-2: B-case-folded == b, D-case-folded == d.
    # name > B: bravo, CHARLIE, delta (all sort after 'b' in CI order)
    # name <= delta: Alpha, bravo, CHARLIE, delta
    # intersection: bravo, CHARLIE, delta
    assert found == ["CHARLIE", "bravo", "delta"]

    plan = coll.find(
        {"name": {"$gt": "B", "$lte": "delta"}},
        collation={"locale": "en", "strength": 2},
    ).explain()
    assert _input_stage(plan) == "IXSCAN"


def test_in_query_via_collation_index(client) -> None:
    coll = client["coll_db"]["in_query"]
    coll.insert_many(
        [
            {"_id": 1, "name": "Alice"},
            {"_id": 2, "name": "BOB"},
            {"_id": 3, "name": "carol"},
        ]
    )
    coll.create_index("name", collation={"locale": "en", "strength": 2})

    docs = list(
        coll.find(
            {"name": {"$in": ["alice", "BOB"]}},
            collation={"locale": "en", "strength": 2},
        )
    )
    assert sorted(d["_id"] for d in docs) == [1, 2]


def test_update_via_collation_index_finds_doc(client) -> None:
    """Index-accelerated update_one routes the filter through the
    collation index just like find does."""
    coll = client["coll_db"]["update"]
    coll.insert_one({"_id": 1, "name": "Alice", "v": 1})
    coll.create_index("name", collation={"locale": "en", "strength": 2})

    result = coll.update_one(
        {"name": "alice"},
        {"$set": {"v": 2}},
        collation={"locale": "en", "strength": 2},
    )
    assert result.modified_count == 1
    assert coll.find_one({"_id": 1})["v"] == 2


# ---------------------------------------------------------------------------
# Edge cases
# ---------------------------------------------------------------------------


def test_numericordering_collation_not_index_supported(client) -> None:
    """numericOrdering requires a length-prefixed digit-run encoding
    we don't ship v1; the picker treats it as "no index for this
    collation" and falls back to COLLSCAN. Correctness still holds —
    matches() honors numericOrdering."""
    coll = client["coll_db"]["numeric"]
    coll.insert_many(
        [
            {"_id": 1, "tag": "a2"},
            {"_id": 2, "tag": "a10"},
        ]
    )
    coll.create_index("tag", collation={"locale": "en", "numericOrdering": True})

    plan = coll.find(
        {"tag": "a10"},
        collation={"locale": "en", "numericOrdering": True},
    ).explain()
    # numericOrdering disables index encoding → COLLSCAN.
    assert _plan_stage(plan) == "COLLSCAN"
    # But results are still correct.
    docs = list(
        coll.find(
            {"tag": "a10"},
            collation={"locale": "en", "numericOrdering": True},
        )
    )
    assert [d["_id"] for d in docs] == [2]


def test_unique_index_with_collation_enforces_under_collation(client) -> None:
    """A unique index with strength-2 collation rejects a second
    insert whose value differs only by case."""
    coll = client["coll_db"]["unique"]
    coll.create_index("name", unique=True, collation={"locale": "en", "strength": 2})
    coll.insert_one({"_id": 1, "name": "Alice"})
    # "alice" collation-equals "Alice" → uniqueness violation.
    from pymongo.errors import DuplicateKeyError

    with pytest.raises(DuplicateKeyError):
        coll.insert_one({"_id": 2, "name": "alice"})


def test_two_indexes_different_collations_pick_correct_one(client) -> None:
    """A collection with two indexes on the same field but different
    collations: each query picks the index whose collation matches."""
    coll = client["coll_db"]["multi_index"]
    coll.insert_many([{"_id": i, "name": n} for i, n in enumerate(["Alice", "BOB"])])
    coll.create_index("name", name="name_codepoint")  # no collation
    coll.create_index(
        "name",
        name="name_ci",
        collation={"locale": "en", "strength": 2},
    )

    # No-collation query → picks the no-collation index.
    plan = coll.find({"name": "Alice"}).explain()
    assert _plan_stage(plan) == "FETCH"
    assert plan["queryPlanner"]["winningPlan"]["inputStage"]["indexName"] == "name_codepoint"

    # Strength-2 query → picks the case-insensitive index.
    plan = coll.find({"name": "alice"}, collation={"locale": "en", "strength": 2}).explain()
    assert _plan_stage(plan) == "FETCH"
    assert plan["queryPlanner"]["winningPlan"]["inputStage"]["indexName"] == "name_ci"
