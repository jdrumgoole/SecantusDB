"""Gap tests for RM-155: the order an unsorted `find` returns documents in
for six predicate shapes, where the plan this server derives is not the one
mongod 8.2.11 runs (found by p4-router's `router_shapes.py`, 2026-10-15).

Every expected `_id` list is mongod 8.2.11's, measured 2026-10-15 on a
one-member replica set over the documents below, each index on its own
collection. The documents are the harness's, without the extreme doubles
and the near-equal double/Decimal128 pair (`_id` 10-18), whose order is
RM-152's.
"""

from __future__ import annotations

import datetime
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

pymongo = pytest.importorskip("pymongo")
bson = pytest.importorskip("bson")

D = datetime.datetime(2024, 1, 1)


def _docs() -> list[dict]:
    from bson import Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex

    scalars = [
        0, 1, 2, 5, 5.0, 6, -3, Int64(5), Decimal128("5"), 2.5, float("nan"),
        float("inf"), float("-inf"), 1e-300, -1e-300, 3e250, -3e250,
        Decimal128("0.1"), 0.1,
        "", "a", "s", "s0", "s0x", "s1", "sa", "sab", "t", "S0", "s\x00", "é",
        True, False, None, D, D + datetime.timedelta(days=1),
        ObjectId("0123456789abcdef01234567"),
        {"k": 1}, {"k": 2}, {"k": 1, "j": 2}, {}, Regex("^s"), MinKey(), MaxKey(),
    ]  # fmt: skip
    arrays = [
        [], [5], [5, 6], [6, 7], [[5]], [None], ["s0", 1], [{"k": 1}, {"k": 2}],
        [{"k": 2}], [[]], [2.5, "x"],
    ]  # fmt: skip
    out = []
    for i, v in enumerate(scalars + arrays):
        out.append({"_id": i, "a": v, "b": v, "c": i % 3})
    i = len(out)
    out.append({"_id": i, "c": 1})
    out.append({"_id": i + 1, "a": None, "c": 2})
    i += 2
    for nv in ([{"k": 1}, {"j": 1}], [{"j": 1}], [{"k": 2}, {"k": 3}], {"k": 2}, 5, [{"k": None}]):
        out.append({"_id": i, "n": nv, "c": 0})
        i += 1
    return [d for d in out if not 10 <= d["_id"] <= 18]


INDEXES = {
    "a_asc": ([("a", 1)], {}),
    "a_desc": ([("a", -1)], {}),
    "a_c": ([("a", 1), ("c", 1)], {}),
    "a_desc_c": ([("a", -1), ("c", 1)], {}),
    "a_sparse": ([("a", 1)], {"sparse": True}),
    "a_partial": ([("a", 1)], {"partialFilterExpression": {"c": {"$gte": 1}}}),
}


def _order(client: Any, index: str, query: dict) -> tuple:
    client.drop_database("rm155")
    coll = client.rm155.t
    coll.insert_many(_docs())
    keys, options = INDEXES[index]
    coll.create_index(keys, name=index, **options)
    return mongo_answer(lambda: [d["_id"] for d in coll.find(query)])


def _check(client: Any, cases: list[tuple[str, dict, list[int]]]) -> None:
    got = [(index, query, _order(client, index, query)) for index, query, _ in cases]
    assert got == [(index, query, ("ok", ids)) for index, query, ids in cases]


ALL_BUT_NULL = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29,
                30, 31, 32, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 50,
                51, 52, 53, 54]  # fmt: skip


@pytest.mark.backlog("RM-155")
def test_ne_null_over_a_multikey_index_scans_the_collection(rust_mongo: Any) -> None:
    """mongod 8.2.11: `$ne: null` / `$nin: [null]` over an index that holds
    arrays is a COLLSCAN, so the documents come in collection order."""
    cases = [
        (ix, q, ALL_BUT_NULL)
        for ix in ("a_asc", "a_desc", "a_c", "a_desc_c")
        for q in ({"a": {"$ne": None}}, {"a": {"$nin": [None]}})
    ]
    _check(rust_mongo, cases)


@pytest.mark.backlog("RM-155")
def test_type_object_reads_the_object_bracket(rust_mongo: Any) -> None:
    """mongod 8.2.11: `$type: "object"` scans the index over `[{}, [])` and
    returns the documents by value."""
    q = {"a": {"$type": "object"}}
    _check(
        rust_mongo,
        [
            ("a_asc", q, [40, 37, 51, 39, 38, 52]),
            ("a_desc", q, [38, 51, 52, 39, 37, 40]),
            ("a_c", q, [40, 51, 37, 39, 52, 38]),
            ("a_desc_c", q, [51, 52, 38, 39, 37, 40]),
            ("a_sparse", q, [40, 37, 51, 39, 38, 52]),
        ],
    )


@pytest.mark.backlog("RM-155")
def test_elem_match_ne_reads_the_index(rust_mongo: Any) -> None:
    """mongod 8.2.11: `$elemMatch: {$ne: 5}` scans `[MinKey, 5), (5,
    MaxKey]` and rechecks the documents it fetches."""
    q = {"a": {"$elemMatch": {"$ne": 5}}}
    _check(
        rust_mongo,
        [
            ("a_asc", q, [49, 50, 54, 46, 47, 51, 52, 53, 48]),
            ("a_desc", q, [48, 53, 51, 52, 54, 50, 47, 46, 49]),
            ("a_c", q, [49, 50, 54, 46, 47, 51, 52, 53, 48]),
            ("a_desc_c", q, [48, 53, 51, 52, 54, 50, 47, 46, 49]),
        ],
    )


@pytest.mark.backlog("RM-155")
def test_type_date_reads_a_descending_index_latest_first(rust_mongo: Any) -> None:
    """mongod 8.2.11: `$type: "date"` scans the date bracket of a descending
    index, so the later date comes first."""
    q = {"a": {"$type": "date"}}
    _check(rust_mongo, [("a_desc", q, [35, 34]), ("a_desc_c", q, [35, 34])])


@pytest.mark.backlog("RM-155")
def test_regex_with_an_alternation_reads_every_string(rust_mongo: Any) -> None:
    """mongod 8.2.11: `^s|t` has no single prefix, so it is bounded by every
    string (`({}, ""]` on a descending index) and `"t"` comes first."""
    q = {"a": {"$regex": "^s|t"}}
    expected = [27, 26, 25, 24, 23, 22, 50, 29, 21]
    _check(rust_mongo, [("a_desc", q, expected), ("a_desc_c", q, expected)])


@pytest.mark.backlog("RM-155")
def test_exists_over_a_partial_index_the_query_implies(rust_mongo: Any) -> None:
    """mongod 8.2.11: the query implies the partial filter `c >= 1`, so
    `$exists: true` reads the partial index whole, in key order."""
    q = {"$and": [{"a": {"$exists": True}}, {"c": {"$gte": 1}}]}
    expected = [44, 49, 56, 1, 50, 2, 4, 7, 8, 46, 5, 47, 19, 28, 20, 29, 22, 23, 25, 26,
                40, 37, 38, 52, 53, 32, 31, 34, 35, 41, 43]  # fmt: skip
    _check(rust_mongo, [("a_partial", q, expected)])
