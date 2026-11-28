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


@pytest.mark.backlog_fixed("RM-694")
def test_a_not_inside_elem_match_entry_reports_its_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sized the entry for `{a: {$lt: 5}, b: {$elemMatch:
    {$not: {$gt: 3}}}}` over `a_1` and `b_1` at 3350 on macOS arm64; this
    server left `estimatedSizeBytes` out. Its winner reads `b_1`, and mongod
    tags the `ELEM_MATCH_VALUE` itself, not the predicates inside."""
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


@pytest.mark.backlog_fixed("RM-694")
def test_an_elem_match_the_winner_reads_reports_its_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sized the entries for an `$elemMatch` over operators
    that the winning `b_1` scan reads at 3214 (`{$lt: 4}`) and 3318 (`{$gt:
    1, $lt: 4}`) on macOS arm64; this server left `estimatedSizeBytes` out."""
    only_on("darwin", why="RM-341 sizes entries with macOS arm64's layout")
    only_on("arm64", why="RM-341 sizes entries with macOS arm64's layout")

    def sizes(client: Any) -> list:
        out = []
        for inner in ({"$lt": 4}, {"$gt": 1, "$lt": 4}):
            q = {"a": {"$lt": 5}, "b": {"$elemMatch": inner}}
            out += [
                e.get("estimatedSizeBytes")
                for e in _entries(client, ["a", "b"], lambda col, q=q: col.find(q))
            ]
        return out

    assert sizes(mongod) == [3214, 3318]
    assert sizes(rust_mongo) == sizes(mongod)


def _with_d(client: Any, run: Any) -> list:
    client.drop_database("rm341_found")
    col = client.rm341_found.c
    col.insert_many(
        [
            {"_id": i, "a": i % 100, "b": (i // 20) % 100, "c": i % 7, "d": [{"e": i % 5}]}
            for i in range(2000)
        ]
    )
    for keys in ("a", "b", "d.e"):
        col.create_index(keys)
    for _ in range(2):
        list(run(col))
    return list(col.aggregate([{"$planCacheStats": {}}]))


@pytest.mark.backlog_fixed("RM-694")
def test_an_object_elem_match_entry_reports_its_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sizes the entry for `{a: {$lt: 5}, d: {$elemMatch: {e:
    3}}}` over `a_1`, `b_1` and `d.e_1` at 3230 on macOS arm64 (read from
    `a_1`), and at 3656 with `a: {$lt: 50}` (read from `d.e_1`); this server
    left `estimatedSizeBytes` out of both."""
    only_on("darwin", why="RM-341 sizes entries with macOS arm64's layout")
    only_on("arm64", why="RM-341 sizes entries with macOS arm64's layout")
    queries = [
        {"a": {"$lt": 5}, "d": {"$elemMatch": {"e": 3}}},
        {"a": {"$lt": 50}, "d": {"$elemMatch": {"e": 3}}},
    ]

    def sizes(client: Any) -> list:
        return [
            [e.get("estimatedSizeBytes") for e in _with_d(client, lambda col, q=q: col.find(q))]
            for q in queries
        ]

    assert sizes(mongod) == [[3230], [3656]]
    assert sizes(rust_mongo) == sizes(mongod)


@pytest.mark.backlog("RM-694")
def test_an_or_entry_with_a_compound_branch_reports_its_size(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 sizes the entry for `{$or: [{a: 3, c: 2}, {b: 5}], c:
    {$lt: 3}}` over `a_1`, `b_1` and `c_1` at 10381 on macOS arm64; this
    server leaves `estimatedSizeBytes` out (a branch of several
    predicates)."""
    only_on("darwin", why="RM-341 sizes entries with macOS arm64's layout")
    only_on("arm64", why="RM-341 sizes entries with macOS arm64's layout")
    q = {"$or": [{"a": 3, "c": 2}, {"b": 5}], "c": {"$lt": 3}}

    def sizes(client: Any) -> list:
        client.drop_database("rm341_found")
        col = client.rm341_found.c
        col.insert_many(
            [{"_id": i, "a": i % 100, "b": (i // 20) % 100, "c": i % 7} for i in range(2000)]
        )
        for keys in ("a", "b", "c"):
            col.create_index(keys)
        for _ in range(2):
            list(col.find(q))
        return [e.get("estimatedSizeBytes") for e in col.aggregate([{"$planCacheStats": {}}])]

    assert sizes(mongod) == [10381]
    assert sizes(rust_mongo) == sizes(mongod)


@pytest.mark.backlog_fixed("RM-826")
def test_the_plan_cache_holds_the_entries_mongod_holds(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 caches no entry for `{a: {$lt: 5}, b: {$not: {$gt: 3,
    $lt: 9}}}` over `a_1` and `b_1`, and one for `{a: {$lt: 5}, d:
    {$elemMatch: {e: {$lt: 2}, f: null}}}` over `a_1`, `b_1` and `d.e_1`;
    this server caches the first and not the second."""
    queries = [
        {"a": {"$lt": 5}, "b": {"$not": {"$gt": 3, "$lt": 9}}},
        {"a": {"$lt": 5}, "d": {"$elemMatch": {"e": {"$lt": 2}, "f": None}}},
    ]

    def counts(client: Any) -> list:
        return [len(_with_d(client, lambda col, q=q: col.find(q))) for q in queries]

    assert counts(mongod) == [0, 1]
    assert counts(rust_mongo) == counts(mongod)


@pytest.mark.backlog_fixed("RM-826")
def test_an_or_beside_a_clause_is_tried_in_every_combination_of_its_indexes(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 tries four plans for `{$or: [{a: 3}, {b: 5}], c: {$lt:
    3}}` over `a_1`, `b_1`, `a_1_c_1` and `b_1_c_1` (each branch through its
    own index or the compound one with `c` pushed down), so it caches an
    entry; this server planned one and cached none. The rejected plans of
    the other two queries are counted too (measured 2026-11-28: `a_1` beside
    `a_1_c_1` for the `$not` of two operators, which `b_1` and `b_1_c_1`
    cannot serve; and none for an `$elemMatch` of two predicates, read
    through `d.e_1` alone)."""
    q = {"$or": [{"a": 3}, {"b": 5}], "c": {"$lt": 3}}

    def plans(client: Any) -> tuple:
        client.drop_database("rm826")
        col = client.rm826.c
        col.insert_many(
            [
                {"_id": i, "a": i % 100, "b": (i // 20) % 100, "c": i % 7, "d": [{"e": i % 5}]}
                for i in range(2000)
            ]
        )
        for keys in ("a", "b", [("a", 1), ("c", 1)], [("b", 1), ("c", 1)], "d.e"):
            col.create_index(keys)
        out = []
        for filt in (
            q,
            {"a": {"$lt": 5}, "b": {"$not": {"$gt": 3, "$lt": 9}}},
            {"d": {"$elemMatch": {"e": {"$lt": 2}, "f": None}}},
        ):
            reply = client.rm826.command(
                "explain", {"find": "c", "filter": filt}, verbosity="queryPlanner"
            )["queryPlanner"]
            out.append(len(reply.get("rejectedPlans", [])))
        for _ in range(2):
            list(col.find(q))
        out.append(len(list(col.aggregate([{"$planCacheStats": {}}]))))
        return tuple(out)

    assert plans(mongod) == (3, 1, 0, 1)
    assert plans(rust_mongo) == plans(mongod)
