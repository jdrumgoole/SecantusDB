"""Gap tests for the sort key a document carries, found working RM-522
(2026-11-13): what a `$unionWith` passes on, the key a `$densify` sets,
and the stages that keep it.

mongod's `$rank` in a pipeline's own `$_internalSetWindowFields` compares
that key, as `{$meta: "sortKey"}` reads it, so both show it.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.test_rm_found_2026_11_10_rm410 import RANK_ID, _answers

META = {"$addFields": {"m": {"$meta": "sortKey"}}}


@pytest.mark.backlog("RM-630")
def test_a_union_passes_the_sort_key_on(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps the outer documents' sort key through a
    `$unionWith`, and the documents it adds carry their sub-pipeline's key
    to a later `{$meta: "sortKey"}` (the empty one with no `$sort`); a rank
    behind it ranks the outer documents by theirs. This server ends the key
    at the `$unionWith`: every `$meta` reads `[]` and every rank ties."""
    sort = {"$sort": {"k": 1}}
    pipelines = {
        "$meta": [sort, {"$unionWith": "c"}, META, {"$project": {"m": 1}}],
        "$meta, a sub-pipeline $sort": [
            sort,
            {"$unionWith": {"coll": "c", "pipeline": [{"$sort": {"n": 1}}]}},
            META,
            {"$project": {"m": 1}},
        ],
        "a rank, stopped by a $limit": [
            sort,
            {"$unionWith": "c"},
            RANK_ID,
            {"$limit": 6},
            {"$project": {"r": 1}},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-631")
def test_a_densify_sets_the_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$densify` leaves each document the key of the
    field it densified by: `{$meta: "sortKey"}` reads it and a rank behind
    it ranks by it. This server gives `[]` and ties them."""
    densify = {"$densify": {"field": "n", "range": {"step": 1, "bounds": "full"}}}
    pipelines = {
        "$meta": [densify, META, {"$project": {"m": 1}}],
        "a rank": [densify, RANK_ID, {"$project": {"r": 1}}],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-632")
def test_a_graph_lookup_and_a_fill_keep_the_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps a document's sort key through a `$graphLookup`
    and a `$fill`, so a rank behind either ranks by the `$sort` in front.
    This server ends the key at each and ties them."""
    sort = {"$sort": {"k": 1}}
    graph = {
        "$graphLookup": {
            "from": "c",
            "startWith": "$n",
            "connectFromField": "n",
            "connectToField": "_id",
            "as": "g",
        }
    }
    pipelines = {
        "$graphLookup": [sort, graph, RANK_ID, {"$project": {"r": 1}}],
        "$fill": [
            sort,
            {"$fill": {"output": {"a": {"value": 0}}}},
            RANK_ID,
            {"$project": {"r": 1}},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)
