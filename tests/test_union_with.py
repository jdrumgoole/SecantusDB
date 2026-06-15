"""``$unionWith`` — concatenate docs from another collection.

Two spec shapes per mongod:

* Shorthand: ``{$unionWith: "<coll>"}``
* Full form: ``{$unionWith: {coll: "<coll>", pipeline: [<sub-pipeline>]}}``

Behaviour pinned here:

* Outer docs come first, then union docs.
* No deduplication — duplicates across the boundary survive.
* The sub-pipeline runs in a fresh :class:`PipelineContext`; outer
  ``$lookup let`` variables are not visible inside.
* Chained ``$unionWith`` stages accumulate.
* Stages after the union (``$sort``, ``$group``, ``$count``) see the
  combined doc set.
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


# ---------------------------------------------------------------------------
# Spec shapes
# ---------------------------------------------------------------------------


def test_union_with_shorthand_string(client) -> None:
    """``{$unionWith: "coll"}`` adds every doc from coll, no pipeline."""
    db = client["uw_db"]
    db["a"].insert_many([{"_id": 1, "n": 1}, {"_id": 2, "n": 2}])
    db["b"].insert_many([{"_id": 10, "n": 10}, {"_id": 11, "n": 11}])

    docs = list(db["a"].aggregate([{"$unionWith": "b"}]))
    assert sorted(d["_id"] for d in docs) == [1, 2, 10, 11]


def test_union_with_full_form_no_pipeline(client) -> None:
    """``{$unionWith: {coll: "b"}}`` — same as shorthand."""
    db = client["uw_db"]
    db["a"].insert_one({"_id": 1, "n": 1})
    db["b"].insert_one({"_id": 10, "n": 10})

    docs = list(db["a"].aggregate([{"$unionWith": {"coll": "b"}}]))
    assert sorted(d["_id"] for d in docs) == [1, 10]


def test_union_with_full_form_with_pipeline(client) -> None:
    """Sub-pipeline filters / projects the union side independently."""
    db = client["uw_db"]
    db["a"].insert_many([{"_id": 1, "n": 1}, {"_id": 2, "n": 2}])
    db["b"].insert_many(
        [
            {"_id": 10, "n": 10, "tag": "yes"},
            {"_id": 11, "n": 11, "tag": "no"},
            {"_id": 12, "n": 12, "tag": "yes"},
        ]
    )

    pipeline = [
        {
            "$unionWith": {
                "coll": "b",
                "pipeline": [
                    {"$match": {"tag": "yes"}},
                    {"$project": {"_id": 1, "n": 1}},
                ],
            }
        }
    ]
    docs = list(db["a"].aggregate(pipeline))
    assert sorted(d["_id"] for d in docs) == [1, 2, 10, 12]


# ---------------------------------------------------------------------------
# Ordering and dedup semantics
# ---------------------------------------------------------------------------


def test_union_with_preserves_outer_first_then_union(client) -> None:
    """Outer docs come first in the result; mongod doesn't formally
    guarantee this, but it's the documented implementation order."""
    db = client["uw_db"]
    db["outer"].insert_many([{"_id": 1, "src": "o"}, {"_id": 2, "src": "o"}])
    db["inner"].insert_many([{"_id": 10, "src": "i"}, {"_id": 11, "src": "i"}])

    docs = list(db["outer"].aggregate([{"$unionWith": "inner"}]))
    # First two are outer (in their natural _id order via _id walk).
    assert docs[0]["src"] == "o"
    assert docs[1]["src"] == "o"
    # Remaining are inner.
    assert {d["src"] for d in docs[2:]} == {"i"}


def test_union_with_no_deduplication(client) -> None:
    """Duplicate _id values across collections survive — mongod's behaviour."""
    db = client["uw_db"]
    db["a"].insert_one({"_id": 1, "src": "a"})
    db["b"].insert_one({"_id": 1, "src": "b"})  # same _id, different coll

    docs = list(db["a"].aggregate([{"$unionWith": "b"}]))
    assert len(docs) == 2
    assert sorted(d["src"] for d in docs) == ["a", "b"]


# ---------------------------------------------------------------------------
# Chaining + downstream stages
# ---------------------------------------------------------------------------


def test_chained_union_with(client) -> None:
    """Two ``$unionWith`` stages back-to-back accumulate three collections."""
    db = client["uw_db"]
    db["a"].insert_one({"_id": 1})
    db["b"].insert_one({"_id": 2})
    db["c"].insert_one({"_id": 3})

    docs = list(db["a"].aggregate([{"$unionWith": "b"}, {"$unionWith": "c"}]))
    assert sorted(d["_id"] for d in docs) == [1, 2, 3]


def test_union_with_then_group(client) -> None:
    """``$group`` after ``$unionWith`` sees the combined doc set."""
    db = client["uw_db"]
    db["a"].insert_many([{"v": 1}, {"v": 2}])
    db["b"].insert_many([{"v": 3}, {"v": 4}])

    pipeline = [
        {"$unionWith": "b"},
        {"$group": {"_id": None, "total": {"$sum": "$v"}}},
    ]
    docs = list(db["a"].aggregate(pipeline))
    assert docs == [{"_id": None, "total": 10}]


def test_union_with_then_sort_limit(client) -> None:
    """``$sort`` + ``$limit`` after union returns the top-K across both."""
    db = client["uw_db"]
    db["a"].insert_many([{"v": 5}, {"v": 1}])
    db["b"].insert_many([{"v": 8}, {"v": 3}])

    pipeline = [{"$unionWith": "b"}, {"$sort": {"v": -1}}, {"$limit": 2}]
    docs = list(db["a"].aggregate(pipeline))
    assert [d["v"] for d in docs] == [8, 5]


# ---------------------------------------------------------------------------
# Edge cases
# ---------------------------------------------------------------------------


def test_union_with_missing_collection_treated_as_empty(client) -> None:
    """A collection that doesn't exist behaves as if it had no docs.
    mongod's behaviour — non-existent collections are silently empty."""
    db = client["uw_db"]
    db["a"].insert_many([{"_id": 1}, {"_id": 2}])

    docs = list(db["a"].aggregate([{"$unionWith": "doesnotexist"}]))
    assert sorted(d["_id"] for d in docs) == [1, 2]


def test_union_with_empty_outer(client) -> None:
    """Empty outer collection + union side has docs → just the union."""
    db = client["uw_db"]
    db.create_collection("a")  # exists but empty
    db["b"].insert_many([{"_id": 10}, {"_id": 11}])

    docs = list(db["a"].aggregate([{"$unionWith": "b"}]))
    assert sorted(d["_id"] for d in docs) == [10, 11]


def test_union_with_rejects_bad_spec(client) -> None:
    """Numeric spec, missing `coll`, bad `pipeline` type all fail with
    an OperationFailure (the wire-side AggregateError)."""
    from pymongo.errors import OperationFailure

    db = client["uw_db"]
    db["a"].insert_one({"_id": 1})

    with pytest.raises(OperationFailure):
        list(db["a"].aggregate([{"$unionWith": 42}]))

    with pytest.raises(OperationFailure):
        list(db["a"].aggregate([{"$unionWith": {"pipeline": []}}]))  # no coll

    with pytest.raises(OperationFailure):
        list(db["a"].aggregate([{"$unionWith": {"coll": "b", "pipeline": "bad"}}]))
