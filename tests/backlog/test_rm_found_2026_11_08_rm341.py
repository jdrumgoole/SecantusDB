"""Divergences found while sizing plan cache entries (RM-341, 2026-11-08),
each measured against mongod 8.2.11 by comparing `$planCacheStats` entries
over one collection of 2,000 documents."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import only_on


def _entries(client: Any, indexes: list, run: Any) -> list:
    client.drop_database("rm341_found")
    col = client.rm341_found.c
    col.insert_many(
        [{"_id": i, "a": i % 100, "b": (i // 20) % 100, "c": i % 7} for i in range(2000)]
    )
    for keys in indexes:
        col.create_index(keys)
    for _ in range(2):
        list(run(col))
    return list(col.aggregate([{"$planCacheStats": {}}]))


def _stages(stage: Any) -> list:
    out = []
    while isinstance(stage, dict):
        out.append(stage)
        stage = stage.get("inputStage")
    return out


@pytest.mark.backlog_fixed("RM-436")
def test_entries_this_does_not_size_report_their_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 lists `estimatedSizeBytes` for every entry. This server
    left it out where it did not build the entry's `PlanCacheIndexTree`:
    a predicate inside `$elemMatch` (3215 on macOS arm64) and an `$or`
    beside other predicates (4912)."""
    only_on("darwin", why="RM-341 sizes entries with macOS arm64's layout")
    only_on("arm64", why="RM-341 sizes entries with macOS arm64's layout")

    def sizes(client: Any) -> list:
        out = []
        for indexes, q in [
            (["a", "b"], {"a": {"$lt": 5}, "b": {"$elemMatch": {"$lt": 90}}}),
            (["a", "b", "c"], {"$or": [{"a": 3}, {"b": 5}], "c": {"$lt": 3}}),
        ]:
            got = _entries(client, indexes, lambda col, q=q: col.find(q))
            out.append([e.get("estimatedSizeBytes") for e in got])
        return out

    assert sizes(mongod) == [[3215], [4912]]
    assert sizes(rust_mongo) == sizes(mongod)


@pytest.mark.backlog_fixed("RM-436")
def test_more_or_and_elem_match_entries_report_their_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's sizes for an `OR` of three branches over two indexes
    (4956), branches of ranges and `$in` (4979), and an `$elemMatch` of two
    operators (3319), on macOS arm64 (measured 2026-11-14)."""
    only_on("darwin", why="RM-341 sizes entries with macOS arm64's layout")
    only_on("arm64", why="RM-341 sizes entries with macOS arm64's layout")
    cases = [
        (["a", "b", "c"], {"$or": [{"a": 3}, {"b": 5}, {"a": 7}], "c": {"$lt": 3}}),
        (
            ["a", "b", "c"],
            {"$or": [{"a": {"$lt": 2}}, {"b": {"$in": [5, 6]}}], "c": {"$lt": 3}},
        ),
        (["a", "b"], {"a": {"$lt": 5}, "b": {"$elemMatch": {"$gt": 1, "$lt": 90}}}),
    ]

    def sizes(client: Any) -> list:
        return [
            [
                e.get("estimatedSizeBytes")
                for e in _entries(client, ix, lambda col, q=q: col.find(q))
            ]
            for ix, q in cases
        ]

    assert sizes(mongod) == [[4956], [4979], [3319]]
    assert sizes(rust_mongo) == sizes(mongod)


@pytest.mark.backlog("RM-694")
def test_a_not_inside_elem_match_entry_reports_its_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sizes the entry for `{a: {$lt: 5}, b: {$elemMatch:
    {$not: {$gt: 3}}}}` over `a_1` and `b_1` at 3350 on macOS arm64; this
    server leaves `estimatedSizeBytes` out."""
    only_on("darwin", why="RM-341 sizes entries with macOS arm64's layout")
    only_on("arm64", why="RM-341 sizes entries with macOS arm64's layout")
    q = {"a": {"$lt": 5}, "b": {"$elemMatch": {"$not": {"$gt": 3}}}}

    def sizes(client: Any) -> list:
        return [
            e.get("estimatedSizeBytes")
            for e in _entries(client, ["a", "b"], lambda col: col.find(q))
        ]

    assert sizes(mongod) == [3350]
    assert sizes(rust_mongo) == sizes(mongod)


@pytest.mark.backlog_fixed("RM-437")
def test_a_sort_merge_candidate_counts_as_mongods(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's winning SORT_MERGE for `{a: {$in: [1, 2]}, b: {$lt:
    90}}` sorted by `c` over `a_1_c_1` and `b_1` reports 82 `works` and its
    `dupsTested` and `dupsDropped` in `creationExecStats`; this server's
    reported 83 works -- its FETCH worked the merge once more after the
    merge was at its end -- and neither field."""

    def merge(client: Any) -> list:
        got = _entries(
            client,
            [[("a", 1), ("c", 1)], "b"],
            lambda col: col.find({"a": {"$in": [1, 2]}, "b": {"$lt": 90}}).sort("c", 1),
        )
        stages = _stages(got[0]["creationExecStats"][0]["executionStages"])
        return [
            {k: v for k, v in s.items() if k not in ("executionTimeMillisEstimate", "inputStages")}
            for s in stages
            if s["stage"] == "SORT_MERGE"
        ]

    assert merge(rust_mongo) == merge(mongod)


def _losers(client: Any, q: dict) -> list:
    got = _entries(client, ["a", "b"], lambda col: col.find(q))
    out = []
    for c in got[0]["creationExecStats"][1:]:
        out.append(
            [
                {
                    k: v
                    for k, v in s.items()
                    if k not in ("executionTimeMillisEstimate", "inputStage")
                }
                for s in _stages(c["executionStages"])
            ]
        )
    return out


@pytest.mark.backlog_fixed("RM-438")
def test_a_nin_candidate_reads_mongods_keys(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's losing candidate for `{a: {$lt: 5}, b: {$nin: [3,
    4]}}` -- `b_1` read over three intervals -- examines 101 keys in the
    trial, seeking 3 times and returning 99; this server's sought twice and
    returned 100, until a seek to the next interval landed at its start
    (RM-433's fix)."""
    q = {"a": {"$lt": 5}, "b": {"$nin": [3, 4]}}
    assert _losers(rust_mongo, q) == _losers(mongod, q)


@pytest.mark.backlog_fixed("RM-439")
@pytest.mark.parametrize(
    "q",
    [
        {"a": {"$lt": 5}, "b": {"$exists": True}},
        {"a": {"$lt": 5}, "b": {"$exists": True}, "c": {"$gt": 1}},
        {"c": {"$gt": 1}, "b": {"$regex": "x"}, "a": {"$lt": 5}},
        {"a": {"$lt": 5}, "b": {"$exists": False}},
    ],
)
def test_an_exists_candidates_filter_is_in_mongods_order(
    rust_mongo: Any, mongod: Any, q: dict
) -> None:
    """mongod 8.2.11's losing candidate for `{a: {$lt: 5}, b: {$exists:
    true}}` over `b_1` rechecks `{$and: [{b: {$exists: true}}, {a: {$lt:
    5}}]}` in its FETCH -- the inexact `$exists`, tagged with the index,
    first (`sortUsingTags`); this server put `a` first."""
    assert _losers(rust_mongo, q) == _losers(mongod, q)


@pytest.mark.backlog_fixed("RM-440")
def test_a_type_candidate_reads_mongods_keys(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's losing candidate for `{a: {$lt: 5}, b: {$type:
    "int"}}` over `b_1` rechecks `$type` against each key and returns all
    101 keys it examines (every `b` is an int) to its FETCH in the trial;
    this server's returned 6 -- the documents the FETCH keeps -- because it
    answered the scan's key filter with whether the whole query matched."""
    q = {"a": {"$lt": 5}, "b": {"$type": "int"}}
    assert _losers(rust_mongo, q) == _losers(mongod, q)


@pytest.mark.backlog_fixed("RM-441")
def test_a_multikey_index_covers_a_field_that_holds_no_array(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers `find({a: {$lt: 5}}, {a: 1, _id: 0})` from
    `a_1_c_1` with a PROJECTION_COVERED over the IXSCAN although `c` holds
    an array in one document: `a` holds none, so its keys are its values.
    This server fetched each document instead; the covered answer must
    still be the fetched one."""

    def plan(client: Any) -> list:
        client.drop_database("rm341_found")
        col = client.rm341_found.c
        col.insert_many([{"_id": i, "a": i % 100, "c": i % 7} for i in range(2000)])
        col.insert_one({"_id": -1, "c": [1, 2]})
        col.create_index([("a", 1), ("c", 1)])
        e = col.find({"a": {"$lt": 5}}, {"a": 1, "_id": 0}).explain()
        rows = sorted(d["a"] for d in col.find({"a": {"$lt": 5}}, {"a": 1, "_id": 0}))
        return [s["stage"] for s in _stages(e["queryPlanner"]["winningPlan"])], rows

    assert plan(mongod)[0] == ["PROJECTION_COVERED", "IXSCAN"]
    assert plan(rust_mongo) == plan(mongod)
