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


@pytest.mark.backlog_fixed("RM-630")
def test_a_union_passes_the_sort_key_on(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps the outer documents' sort key through a
    `$unionWith`, and the documents it adds carry their sub-pipeline's key
    to a later `{$meta: "sortKey"}` (the empty one with no `$sort`); a rank
    behind it ranks the outer documents by theirs. This server ended the key
    at the `$unionWith`: every `$meta` read `[]` and every rank tied."""
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


@pytest.mark.backlog_fixed("RM-631")
def test_a_densify_sets_the_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$densify` leaves each document the key of the
    field it densified by: `{$meta: "sortKey"}` reads it and a rank behind
    it ranks by it. This server gave `[]` and tied them."""
    densify = {"$densify": {"field": "n", "range": {"step": 1, "bounds": "full"}}}
    pipelines = {
        "$meta": [densify, META, {"$project": {"m": 1}}],
        "a rank": [densify, RANK_ID, {"$project": {"r": 1}}],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog_fixed("RM-632")
def test_a_graph_lookup_and_a_fill_keep_the_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 keeps a document's sort key through a `$graphLookup`
    and a `$fill`, so a rank behind either ranks by the `$sort` in front.
    This server ended the key at each and tied them."""
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


@pytest.mark.backlog_fixed("RM-632")
def test_a_fill_by_a_method_sets_its_windows_sort_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs a `$fill` that fills by a `method` as a window on
    its partition and `sortBy`, and `{$meta: "sortKey"}` behind it reads that
    window's key; a `$fill` of values only keeps the `$sort`'s. This server
    ended the key at every `$fill`. Compared by `_id`: the order among
    equal keys behind a `$fill` is RM-767's."""
    sort = {"$sort": {"k": 1}}
    project = {"$project": {"m": 1}}
    fills = {
        "sortBy": {"sortBy": {"n": 1}, "output": {"a": {"method": "locf"}}},
        "partitionByFields": {
            "partitionByFields": ["n"],
            "sortBy": {"_id": -1},
            "output": {"a": {"method": "locf"}},
        },
        "partitionBy": {
            "partitionBy": {"$mod": ["$n", 2]},
            "sortBy": {"_id": 1},
            "output": {"a": {"method": "locf"}},
        },
        "values only": {"sortBy": {"n": 1}, "output": {"a": {"value": 1}}},
    }
    pipelines = {name: [sort, {"$fill": f}, META, project] for name, f in fills.items()}

    def by_id(client: Any) -> dict:
        return {
            name: (status, sorted(docs, key=lambda d: d["_id"]) if status == "ok" else docs)
            for name, (status, docs) in _answers(client, pipelines).items()
        }

    assert by_id(rust_mongo) == by_id(mongod)


@pytest.mark.backlog("RM-767")
def test_a_fill_passes_ties_on_in_mongods_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 passes the documents a `$fill` fills on, among equal
    `sortBy` keys, in the order it was handed them; this server gives a
    different order among the ties (with values only and by a `method`)."""
    fills = {
        "values only": {"sortBy": {"n": 1}, "output": {"a": {"value": 1}}},
        "locf": {"sortBy": {"n": 1}, "output": {"a": {"method": "locf"}}},
    }
    pipelines = {
        name: [{"$sort": {"k": 1}}, {"$fill": f}, {"$project": {"_id": 1}}]
        for name, f in fills.items()
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)
