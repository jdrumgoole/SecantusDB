"""``$setWindowFields`` rank functions: ``$rank`` / ``$denseRank`` /
``$documentNumber``.

These three sit alongside the accumulator functions in
``$setWindowFields.output``, but evaluate differently: they don't
take an ``window`` argument (mongod rejects it), they take no
function argument either (the spec is just ``{$rank: {}}``), and
they're computed once per partition slot rather than rolled up
over a windowed subset.

The rank computation runs in one linear walk per partition:

* ``$documentNumber`` — 1-indexed slot position, independent of
  ties.
* ``$rank`` — 1-indexed position with **gaps**: tied rows share the
  lower rank, next non-tied row jumps by the number of ties
  (``[10, 20, 20, 30]`` → ``[1, 2, 2, 4]``).
* ``$denseRank`` — 1-indexed position **without gaps**: tied rows
  share, next row is +1 (``[10, 20, 20, 30]`` → ``[1, 2, 2, 3]``).

``$rank`` and ``$denseRank`` require ``sortBy`` (without it every
row would tautologically be "tied"). ``$documentNumber`` is happy
without ``sortBy``.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

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
# $documentNumber
# ---------------------------------------------------------------------------


def test_document_number_with_sort(client) -> None:
    """1-indexed position within partition, after sortBy."""
    coll = client["rank_db"]["docnum"]
    coll.insert_many([{"_id": 1, "v": 30}, {"_id": 2, "v": 10}, {"_id": 3, "v": 20}])

    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"v": 1},
                "output": {"n": {"$documentNumber": {}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # In v-asc order: v=10 (_id=2, n=1), v=20 (_id=3, n=2), v=30 (_id=1, n=3).
    assert [(d["_id"], d["n"]) for d in docs] == [(1, 3), (2, 1), (3, 2)]


def test_document_number_without_sort(client) -> None:
    """``$documentNumber`` works without sortBy — input order is the
    partition order."""
    coll = client["rank_db"]["docnum_nosort"]
    coll.insert_many([{"_id": i} for i in range(5)])

    pipeline = [
        {"$setWindowFields": {"output": {"n": {"$documentNumber": {}}}}},
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # _id-asc is the natural input order in storage, so n matches _id+1.
    assert [d["n"] for d in docs] == [1, 2, 3, 4, 5]


def test_document_number_per_partition(client) -> None:
    """Rank counters reset at each partition boundary."""
    coll = client["rank_db"]["docnum_part"]
    coll.insert_many(
        [
            {"_id": 1, "cat": "a", "v": 30},
            {"_id": 2, "cat": "a", "v": 10},
            {"_id": 3, "cat": "b", "v": 100},
            {"_id": 4, "cat": "b", "v": 50},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "partitionBy": "$cat",
                "sortBy": {"v": 1},
                "output": {"n": {"$documentNumber": {}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # cat=a sorted by v: v=10 (_id=2, n=1), v=30 (_id=1, n=2).
    # cat=b sorted by v: v=50 (_id=4, n=1), v=100 (_id=3, n=2).
    assert [(d["_id"], d["n"]) for d in docs] == [(1, 2), (2, 1), (3, 2), (4, 1)]


# ---------------------------------------------------------------------------
# $rank — gaps after ties
# ---------------------------------------------------------------------------


def test_rank_with_ties_has_gaps(client) -> None:
    """``$rank`` produces gaps after ties: [10, 20, 20, 30] → [1, 2, 2, 4]."""
    coll = client["rank_db"]["rank_ties"]
    coll.insert_many(
        [
            {"_id": 1, "v": 10},
            {"_id": 2, "v": 20},
            {"_id": 3, "v": 20},
            {"_id": 4, "v": 30},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"v": 1},
                "output": {"r": {"$rank": {}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # Sorted by v: v=10 (_id=1, rank=1), v=20 (_id=2, rank=2), v=20 (_id=3, rank=2),
    # v=30 (_id=4, rank=4). Note _id=2 and _id=3 both rank 2; _id=4 jumps to 4.
    assert [(d["_id"], d["r"]) for d in docs] == [(1, 1), (2, 2), (3, 2), (4, 4)]


def test_rank_no_ties_matches_document_number(client) -> None:
    """Without ties, ``$rank`` == ``$documentNumber``."""
    coll = client["rank_db"]["rank_unique"]
    coll.insert_many([{"_id": i, "v": i * 10} for i in range(1, 4)])

    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"v": 1},
                "output": {"r": {"$rank": {}}, "n": {"$documentNumber": {}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    for d in docs:
        assert d["r"] == d["n"]


def test_rank_compound_sort_key_tie_detection(client) -> None:
    """Ties on a compound sort spec: both (a, b) values must match."""
    coll = client["rank_db"]["rank_compound"]
    coll.insert_many(
        [
            {"_id": 1, "a": 1, "b": 5},
            {"_id": 2, "a": 1, "b": 5},  # tied with _id=1 on (a, b)
            {"_id": 3, "a": 1, "b": 10},  # different b → not tied
            {"_id": 4, "a": 2, "b": 5},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"a": 1, "b": 1},
                "output": {"r": {"$rank": {}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # Sorted by (a, b): _id=1(1,5)=1, _id=2(1,5)=1, _id=3(1,10)=3, _id=4(2,5)=4
    assert [(d["_id"], d["r"]) for d in docs] == [(1, 1), (2, 1), (3, 3), (4, 4)]


# ---------------------------------------------------------------------------
# $denseRank — no gaps after ties
# ---------------------------------------------------------------------------


def test_dense_rank_with_ties_has_no_gaps(client) -> None:
    """``$denseRank``: [10, 20, 20, 30] → [1, 2, 2, 3] (no jump)."""
    coll = client["rank_db"]["dense"]
    coll.insert_many(
        [
            {"_id": 1, "v": 10},
            {"_id": 2, "v": 20},
            {"_id": 3, "v": 20},
            {"_id": 4, "v": 30},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"v": 1},
                "output": {"d": {"$denseRank": {}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    assert [(d["_id"], d["d"]) for d in docs] == [(1, 1), (2, 2), (3, 2), (4, 3)]


def test_rank_dense_rank_doc_number_together(client) -> None:
    """All three rank functions in one stage — single linear walk
    computes them in one pass."""
    coll = client["rank_db"]["all_three"]
    coll.insert_many(
        [
            {"_id": 1, "v": 10},
            {"_id": 2, "v": 20},
            {"_id": 3, "v": 20},
            {"_id": 4, "v": 30},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "sortBy": {"v": 1},
                "output": {
                    "n": {"$documentNumber": {}},
                    "r": {"$rank": {}},
                    "d": {"$denseRank": {}},
                },
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # v=10 → n=1, r=1, d=1
    # v=20 → n=2, r=2, d=2
    # v=20 → n=3, r=2, d=2
    # v=30 → n=4, r=4, d=3
    expected = [
        {"_id": 1, "n": 1, "r": 1, "d": 1},
        {"_id": 2, "n": 2, "r": 2, "d": 2},
        {"_id": 3, "n": 3, "r": 2, "d": 2},
        {"_id": 4, "n": 4, "r": 4, "d": 3},
    ]
    for got, exp in zip(docs, expected, strict=True):
        for k, v in exp.items():
            assert got[k] == v, f"{got['_id']} {k}={got[k]} expected {v}"


def test_rank_with_partition(client) -> None:
    """Rank counters reset at partition boundaries."""
    coll = client["rank_db"]["rank_part"]
    coll.insert_many(
        [
            {"_id": 1, "cat": "a", "v": 10},
            {"_id": 2, "cat": "a", "v": 20},
            {"_id": 3, "cat": "a", "v": 20},
            {"_id": 4, "cat": "b", "v": 100},
            {"_id": 5, "cat": "b", "v": 100},
            {"_id": 6, "cat": "b", "v": 200},
        ]
    )
    pipeline = [
        {
            "$setWindowFields": {
                "partitionBy": "$cat",
                "sortBy": {"v": 1},
                "output": {"r": {"$rank": {}}},
            }
        },
        {"$sort": {"_id": 1}},
    ]
    docs = list(coll.aggregate(pipeline))
    # cat=a: v=10 r=1, v=20 r=2, v=20 r=2
    # cat=b: v=100 r=1, v=100 r=1, v=200 r=3
    assert [(d["_id"], d["r"]) for d in docs] == [(1, 1), (2, 2), (3, 2), (4, 1), (5, 1), (6, 3)]


# ---------------------------------------------------------------------------
# Validation: window not allowed; sortBy required for $rank / $denseRank
# ---------------------------------------------------------------------------


def test_rank_with_window_rejected(client) -> None:
    coll = client["rank_db"]["rank_window"]
    coll.insert_one({"_id": 1, "v": 1})

    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$setWindowFields": {
                            "sortBy": {"v": 1},
                            "output": {"r": {"$rank": {}, "window": {"documents": [-1, 1]}}},
                        }
                    }
                ]
            )
        )


def test_rank_without_sort_rejected(client) -> None:
    coll = client["rank_db"]["rank_nosort"]
    coll.insert_one({"_id": 1, "v": 1})

    with pytest.raises(OperationFailure):
        list(coll.aggregate([{"$setWindowFields": {"output": {"r": {"$rank": {}}}}}]))


def test_dense_rank_without_sort_rejected(client) -> None:
    coll = client["rank_db"]["dense_nosort"]
    coll.insert_one({"_id": 1, "v": 1})

    with pytest.raises(OperationFailure):
        list(coll.aggregate([{"$setWindowFields": {"output": {"d": {"$denseRank": {}}}}}]))


def test_rank_with_non_empty_arg_rejected(client) -> None:
    """``$rank`` takes no parameters — non-empty arg is a parse error."""
    coll = client["rank_db"]["rank_arg"]
    coll.insert_one({"_id": 1, "v": 1})

    with pytest.raises(OperationFailure):
        list(
            coll.aggregate(
                [
                    {
                        "$setWindowFields": {
                            "sortBy": {"v": 1},
                            "output": {"r": {"$rank": "$v"}},
                        }
                    }
                ]
            )
        )
