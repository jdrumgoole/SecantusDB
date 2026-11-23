"""Gap tests found working RM-635 (2026-11-22): what a stored BSON
`undefined` still does that mongod 8.2.11's does not.

pymongo cannot encode `undefined`, so the documents are raw BSON.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_found_2026_11_13_rm525 import DOCS, SORT


def _answers(client: Any, pipelines: dict[str, list]) -> dict:
    client.drop_database("rmfound1122rm635")
    db = client.rmfound1122rm635
    db.c.insert_many(DOCS)
    return {
        name: mongo_answer(lambda p=p: list(db.c.aggregate(p))) for name, p in pipelines.items()
    }


@pytest.mark.backlog_fixed("RM-761")
def test_arithmetic_over_a_stored_undefined_is_null(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 treats a stored `undefined` as null in `$add`: the sum
    is null. This server refused it with code 14 ("$add only supports
    numeric or date types"; RM-761, closed by RM-763's MISSING case)."""
    pipelines = {
        "$add": [SORT, {"$project": {"t": {"$add": ["$a", 1]}}}],
        "$multiply": [SORT, {"$project": {"t": {"$multiply": ["$a", 2]}}}],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


def _let_pipeline(ids: list[int]) -> list:
    return [
        {"$match": {"_id": {"$in": ids}}},
        SORT,
        {
            "$lookup": {
                "from": "c",
                "let": {"v": "$a"},
                "pipeline": [{"$limit": 1}, {"$project": {"t": {"$type": "$$v"}}}],
                "as": "l",
            }
        },
        {"$project": {"l": 1}},
    ]


@pytest.mark.backlog_fixed("RM-762")
def test_a_lookup_let_variable_keeps_a_stored_undefined(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$lookup` `let` variable bound from a stored
    `undefined` reads "undefined" in `$type` (and one bound from null reads
    "null"). This server read "missing" (RM-762, closed by RM-763)."""
    pipelines = {"$lookup let": _let_pipeline([1, 2])}
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog_fixed("RM-763")
def test_a_lookup_let_variable_bound_from_an_absent_field_is_missing(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11's `$lookup` `let` variable bound from an absent field
    reads "missing" in `$type`. This server bound null: the variables of a
    sub-pipeline were a document of values, which has no MISSING. They are
    a `Vars` now, a `Field` per name (RM-763)."""
    pipelines = {"$lookup let": _let_pipeline([3])}
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


# ------------------------- where a variable crosses a stage boundary (RM-763) --

_T = {"$project": {"t": {"$type": "$$v"}, "w": "$$v"}}
_IS_MISSING = {"$expr": {"$eq": [{"$type": "$$v"}, "missing"]}}


def _crossings(client: Any) -> dict:
    client.drop_database("rm763b")
    db = client.rm763b
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2}])
    db.d.insert_many([{"_id": 10}, {"_id": 11}])

    def agg(pipeline: list, **kw: Any) -> tuple:
        return mongo_answer(lambda: list(db.c.aggregate(pipeline, **kw)))

    def lookup_in(sub: list, doc_id: int = 2) -> list:
        return [
            {"$match": {"_id": doc_id}},
            {"$lookup": {"from": "c", "let": {"v": "$a"}, "pipeline": sub, "as": "l"}},
            {"$project": {"l": 1}},
        ]

    union = {"$unionWith": {"coll": "d", "pipeline": [{"$limit": 1}, _T]}}
    graph = {
        "$graphLookup": {
            "from": "c",
            "startWith": 1,
            "connectFromField": "a",
            "connectToField": "a",
            "as": "g",
            "restrictSearchWithMatch": _IS_MISSING,
        }
    }
    out: dict = {}
    for name, let in (("remove", "$$REMOVE"), ("const", 5)):
        out[f"command let {name}"] = agg([{"$match": {"_id": 1}}, _T], let={"v": let})
        out[f"facet, command let {name}"] = agg(
            [{"$facet": {"f": [{"$limit": 1}, _T]}}], let={"v": let}
        )
        out[f"unionWith, command let {name}"] = agg([{"$limit": 1}, union], let={"v": let})
        out[f"graphLookup, command let {name}"] = agg(
            [{"$limit": 1}, graph, {"$project": {"g": 1}}], let={"v": let}
        )
    out["find let"] = mongo_answer(
        lambda: list(db.c.find(_IS_MISSING, let={"v": "$$REMOVE"}).sort("_id"))
    )
    out["unionWith in a lookup"] = agg(lookup_in([{"$limit": 1}, union]))
    out["unionWith in a lookup, bound"] = agg(lookup_in([{"$limit": 1}, union], 1))
    out["facet in a lookup"] = agg(lookup_in([{"$facet": {"f": [{"$limit": 1}, _T]}}]))
    out["lookup in a lookup"] = agg(
        lookup_in(
            [
                {"$limit": 1},
                {"$lookup": {"from": "c", "pipeline": [{"$limit": 1}, _T], "as": "m"}},
            ]
        )
    )
    out["lookup let from a variable"] = agg(
        lookup_in(
            [
                {"$limit": 1},
                {
                    "$lookup": {
                        "from": "c",
                        "let": {"w": "$$v"},
                        "pipeline": [{"$limit": 1}, {"$project": {"t": {"$type": "$$w"}}}],
                        "as": "m",
                    }
                },
            ]
        )
    )
    out["ifNull and eq"] = agg(
        lookup_in(
            [
                {"$limit": 1},
                {
                    "$project": {
                        "t": {"$ifNull": ["$$v", "dflt"]},
                        "e": {"$eq": ["$$v", None]},
                    }
                },
            ]
        )
    )
    out["graphLookup in a lookup"] = agg(
        lookup_in(
            [
                {
                    "$graphLookup": {
                        **graph["$graphLookup"],
                        "restrictSearchWithMatch": _IS_MISSING,
                    }
                },
                {"$project": {"g": 1}},
            ]
        )
    )

    def update_statement() -> Any:
        db.out.drop()
        db.out.insert_one({"_id": 1})
        db.command(
            "update",
            "out",
            updates=[{"q": {"_id": 1}, "u": [{"$set": {"t": {"$type": "$$v"}, "w": "$$v"}}]}],
            let={"v": "$$REMOVE"},
        )
        return list(db.out.find())

    out["update let"] = mongo_answer(update_statement)
    out["findAndModify let"] = mongo_answer(
        lambda: db.command(
            "findAndModify",
            "c",
            query={"_id": 1, **_IS_MISSING},
            update=[{"$set": {"t": {"$type": "$$v"}}}],
            let={"v": "$$REMOVE"},
            new=True,
        )["value"]
    )
    return out


@pytest.mark.backlog_fixed("RM-763")
def test_a_variable_keeps_its_binding_across_every_stage_boundary(
    rust_mongo: Any, mongod: Any
) -> None:
    """A variable crosses into a `$facet`, a `$unionWith` (which has no `let`
    of its own and sees the stage's scope: this server passed it `$$NOW`
    alone), a `$graphLookup`'s `restrictSearchWithMatch` (which this server
    ran with no variables at all), a nested `$lookup`, an update and a
    `findAndModify`, as mongod 8.2.11 passes it: bound from `$$REMOVE` or an
    absent field it reads "missing" in each, and a constant reads its value."""
    assert _crossings(rust_mongo) == _crossings(mongod)


def _merge_let(client: Any, let: Any, doc_id: int) -> tuple:
    client.drop_database("rm763m")
    db = client.rm763m
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2}])
    db.out.insert_one({"_id": doc_id})
    stage = {
        "into": "out",
        "let": let,
        "whenMatched": [{"$set": {"t": {"$type": "$$x"}, "v": "$$x"}}],
        "whenNotMatched": "insert",
    }
    return mongo_answer(
        lambda: (
            list(db.c.aggregate([{"$match": {"_id": doc_id}}, {"$merge": stage}])),
            list(db.out.find()),
        )[1]
    )


@pytest.mark.backlog_fixed("RM-763")
def test_a_merge_let_variable_bound_to_missing_is_undefined(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 hands a `$merge` `whenMatched` pipeline the stage's
    `let`, but one bound from an absent field or `$$REMOVE` is not among its
    variables there: 17276 "Use of undefined variable: x"."""
    for let, doc_id in (({"x": "$a"}, 1), ({"x": "$a"}, 2), ({"x": "$$REMOVE"}, 1), ({"x": 5}, 1)):
        assert _merge_let(rust_mongo, let, doc_id) == _merge_let(mongod, let, doc_id), (let, doc_id)
