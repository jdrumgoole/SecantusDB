"""Gap and regression tests for what mongod 8.2 added and the Rust MongoDB
server lacked (found 2026-10-20, measured against mongod 8.2.11)."""

from __future__ import annotations

import json
from typing import Any

import pytest
from bson import Decimal128, Int64, json_util

from tests.backlog.conftest import mongo_answer

EXPRESSIONS: list[Any] = [
    {"$sigmoid": 0},
    {"$sigmoid": -1000},
    {"$sigmoid": Int64(2)},
    {"$sigmoid": Decimal128("2")},
    {"$sigmoid": "$x"},
    {"$sigmoid": "$s"},
    {"$sigmoid": "$missing"},
    {"$sigmoid": None},
    {"$sigmoid": "x"},
    {"$sigmoid": [1]},
    {"$sigmoid": True},
    {"$sigmoid": {"$add": [1, 2]}},
    {"$similarityCosine": [[1, 2], [1, 2]]},
    {"$similarityCosine": [[1.5, 2.5], [0.5, 1]]},
    {"$similarityCosine": [[0, 0], [1, 1]]},
    {"$similarityCosine": [[], []]},
    {"$similarityCosine": ["$v", [3, 4]]},
    {"$similarityCosine": {"vectors": [[1, 2], [2, 1]], "score": True}},
    {"$similarityDotProduct": [[1, Decimal128("2")], [1, 2]]},
    {"$similarityDotProduct": {"vectors": [[1, 2], [2, 1]], "score": True}},
    {"$similarityEuclidean": [[1, 2], [4, 6]]},
    {"$similarityEuclidean": {"vectors": [[1, 2], [2, 1]], "score": True}},
    {"$similarityCosine": [[1, 2], None]},
    {"$similarityCosine": [[1, 2], [3]]},
    {"$similarityCosine": [[1, "a"], [1, 1]]},
    {"$similarityCosine": [[1, 2], [1, "a"]]},
    {"$similarityCosine": ["$x", [1]]},
    {"$similarityCosine": [[1], "$x"]},
    {"$similarityCosine": [[1, 2]]},
    {"$similarityCosine": {"vectors": [[1], [1]], "bogus": 1}},
    {"$similarityCosine": {"vectors": [[1], [1]], "score": 1}},
    {"$similarityCosine": {"vectors": 5}},
    {"$similarityCosine": 5},
]


def _answers(client: Any) -> list:
    db = client["rm197"]
    db.drop_collection("s")
    db.s.insert_one({"_id": 1, "x": 2, "s": "str", "v": [1, 2]})
    out = []
    for e in EXPRESSIONS:
        got = mongo_answer(
            lambda e=e: [d.get("r") for d in db.s.aggregate([{"$project": {"_id": 0, "r": e}}])]
        )
        out.append(json.loads(json_util.dumps(got)))
    return out


@pytest.mark.backlog_fixed("RM-197")
def test_expressions_82_added(rust_mongo: Any, mongod: Any) -> None:
    """`$sigmoid` and the three `$similarity*` expressions, their values,
    types and refusals, as mongod 8.2.11 answers them."""
    assert _answers(rust_mongo) == _answers(mongod)


def _sigmoid_explained(client: Any) -> Any:
    db = client["rm197"]
    db.drop_collection("e")
    db.e.insert_one({"x": 1})
    reply = db.command(
        "explain",
        {
            "aggregate": "e",
            "pipeline": [{"$project": {"_id": 0, "r": {"$sigmoid": "$x"}}}],
            "cursor": {},
        },
    )
    return reply["queryPlanner"]["winningPlan"]["transformBy"]


@pytest.mark.backlog_fixed("RM-197")
def test_sigmoid_is_explained_as_mongod_rewrites_it(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 rewrites `$sigmoid` into `$divide` / `$add` / `$exp` /
    `$multiply` when it parses it, and `explain` prints the rewrite."""
    assert _sigmoid_explained(rust_mongo) == _sigmoid_explained(mongod)


def _gated(client: Any) -> list:
    db = client["rm197g"]
    db.drop_collection("s")
    db.s.insert_one({"x": 1})
    client.admin.command("setFeatureCompatibilityVersion", "8.0", confirm=True)
    try:
        return [
            mongo_answer(
                lambda op=op, arg=arg: list(
                    db.s.aggregate([{"$project": {"_id": 0, "r": {op: arg}}}])
                )
            )
            for op, arg in (
                ("$sigmoid", 1),
                ("$similarityCosine", [[1], [1]]),
                ("$similarityDotProduct", [[1], [1]]),
                ("$similarityEuclidean", [[1], [1]]),
            )
        ]
    finally:
        client.admin.command("setFeatureCompatibilityVersion", "8.2", confirm=True)


@pytest.mark.backlog_fixed("RM-025")
def test_fcv_80_refuses_every_expression_82_added(rust_mongo: Any, mongod: Any) -> None:
    """At FCV 8.0, mongod 8.2.11 refuses each expression 8.2 added with 224
    QueryFeatureNotAllowed."""
    assert _gated(rust_mongo) == _gated(mongod)


def _stages(client: Any) -> list:
    db = client["rm198"]
    db.drop_collection("s")
    db.s.insert_many([{"_id": i, "x": i, "y": (i * 7) % 5} for i in range(6)])
    pipelines = [
        [
            {
                "$rankFusion": {
                    "input": {
                        "pipelines": {"a": [{"$sort": {"x": 1}}], "b": [{"$sort": {"y": -1}}]}
                    }
                }
            },
            {"$project": {"_id": 1, "s": {"$meta": "score"}}},
        ],
        [
            {"$score": {"score": "$x", "normalization": "sigmoid"}},
            {"$project": {"_id": 1, "s": {"$meta": "score"}}},
        ],
        [
            {
                "$scoreFusion": {
                    "input": {
                        "pipelines": {
                            "a": [{"$score": {"score": "$x"}}],
                            "b": [{"$score": {"score": "$y"}}],
                        },
                        "normalization": "none",
                    }
                }
            },
            {"$project": {"_id": 1, "s": {"$meta": "score"}}},
        ],
    ]
    return [mongo_answer(lambda p=p: list(db.s.aggregate(p))) for p in pipelines]


@pytest.mark.backlog_fixed("RM-198")
def test_hybrid_search_stages_82_added(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs `$rankFusion` (reciprocal rank fusion), `$score`
    and `$scoreFusion`, and exposes the result as `{$meta: "score"}`."""
    assert _stages(rust_mongo) == _stages(mongod)


@pytest.mark.backlog("RM-200")
def test_explain_of_rank_fusion_is_its_desugaring(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains `$rankFusion` as the stages it desugars into."""
    answers = []
    for client in (rust_mongo, mongod):
        db = client["rm200"]
        db.drop_collection("s")
        db.s.insert_many([{"_id": i, "x": i, "y": (i * 7) % 5} for i in range(6)])
        pipeline = [
            {
                "$rankFusion": {
                    "input": {
                        "pipelines": {"a": [{"$sort": {"x": 1}}], "b": [{"$sort": {"y": -1}}]}
                    }
                }
            }
        ]
        reply = db.command("explain", {"aggregate": "s", "pipeline": pipeline, "cursor": {}})
        answers.append([next(iter(stage)) for stage in reply.get("stages", [])])
    got, want = answers
    assert got == want
