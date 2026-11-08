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


@pytest.mark.backlog_fixed("RM-200")
def test_explain_of_rank_fusion_is_its_desugaring(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explained `$rankFusion` as the stages it desugars into,
    where this server listed the stage as written."""
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


# What each listing of RM-200 covers: every normalisation, weights, score
# details and the custom combination, a stage the cursor absorbs past
# `$score`, and the score read back after a fusion (mongod 8.2.11,
# measured 2026-11-08).
HYBRID_LISTINGS: dict[str, list] = {
    "rank_two": [
        {
            "$rankFusion": {
                "input": {"pipelines": {"a": [{"$sort": {"x": 1}}], "b": [{"$sort": {"y": -1}}]}}
            }
        }
    ],
    "rank_details": [
        {
            "$rankFusion": {
                "input": {
                    "pipelines": {
                        "a": [{"$sort": {"x": 1}}],
                        "b": [{"$match": {"y": {"$gt": 0}}}, {"$sort": {"y": -1}}, {"$limit": 3}],
                    }
                },
                "combination": {"weights": {"a": 2}},
                "scoreDetails": True,
            }
        }
    ],
    "rank_unabsorbed": [
        {
            "$rankFusion": {
                "input": {
                    "pipelines": {
                        "a": [{"$sort": {"x": 1}}, {"$skip": 1}, {"$match": {"y": {"$gt": 0}}}]
                    }
                }
            }
        }
    ],
    "rank_then_meta": [
        {"$rankFusion": {"input": {"pipelines": {"a": [{"$sort": {"x": -1}}]}}}},
        {"$addFields": {"z": 1}},
        {"$sort": {"s": {"$meta": "score"}, "x": 1}},
        {"$project": {"x": 1, "s": {"$meta": "score"}}},
    ],
    "score_none": [{"$score": {"score": "$x", "weight": 1}}],
    "score_min_max": [
        {
            "$score": {
                "score": "$x",
                "normalization": "minMaxScaler",
                "weight": 0.5,
                "scoreDetails": True,
            }
        }
    ],
    "score_sigmoid": [
        {
            "$score": {
                "score": {"$add": ["$x", 1]},
                "normalization": "sigmoid",
                "weight": 0.5,
                "scoreDetails": True,
            }
        }
    ],
    "score_paged": [
        {"$match": {"x": {"$gt": 0}}},
        {"$score": {"score": "$x"}},
        {"$limit": 2},
        {"$replaceWith": {"a": "$x", "s": {"$meta": "score"}}},
    ],
    "score_fusion": [
        {
            "$scoreFusion": {
                "input": {
                    "pipelines": {
                        "a": [{"$score": {"score": "$x", "normalization": "sigmoid"}}],
                        "b": [{"$match": {"x": {"$gt": 2}}}, {"$score": {"score": "$y"}}],
                    },
                    "normalization": "none",
                },
                "combination": {"weights": {"b": 3}},
                "scoreDetails": True,
            }
        }
    ],
    "score_fusion_min_max": [
        {
            "$scoreFusion": {
                "input": {
                    "pipelines": {"a": [{"$score": {"score": "$x", "scoreDetails": True}}]},
                    "normalization": "minMaxScaler",
                },
                "scoreDetails": True,
            }
        }
    ],
    "score_fusion_expression": [
        {
            "$scoreFusion": {
                "input": {
                    "pipelines": {
                        "a": [{"$score": {"score": "$x"}}],
                        "b": [{"$score": {"score": "$y"}}],
                    },
                    "normalization": "sigmoid",
                },
                "combination": {"method": "expression", "expression": {"$add": ["$$a", "$$b"]}},
                "scoreDetails": True,
            }
        }
    ],
}

PAGING = {"$cursor", "$match", "$sort", "$limit", "$skip"}


def _trimmed(stages: list) -> list:
    """`stages` with each `$unionWith`'s from where its input pipeline's own
    stages end, in a `$facet` too: what a `$unionWith`'s sub-pipeline shows
    of its cursor is RM-426's, not this item's."""
    out = []
    for stage in stages:
        if "$unionWith" in stage:
            inner = stage["$unionWith"]["pipeline"]
            while inner and next(iter(inner[0])) in PAGING:
                inner = inner[1:]
            stage = {"$unionWith": {**stage["$unionWith"], "pipeline": _trimmed(inner)}}
        elif "$facet" in stage:
            stage = {"$facet": {k: _trimmed(v) for k, v in stage["$facet"].items()}}
        out.append(stage)
    return out


def _listed(stages: list) -> list:
    """The stages after the cursor, trimmed."""
    return _trimmed(stages[1:])


def _hybrid_listings(client: Any) -> dict:
    client.drop_database("rm200l")
    db = client["rm200l"]
    db.s.insert_many([{"_id": i, "x": i, "y": (i * 7) % 5} for i in range(6)])
    out = {}
    for name, pipeline in HYBRID_LISTINGS.items():
        reply = db.command(
            "explain",
            {"aggregate": "s", "pipeline": pipeline, "cursor": {}},
            verbosity="queryPlanner",
        )
        cursor = reply["stages"][0]["$cursor"]["queryPlanner"]
        plan = cursor["winningPlan"]
        out[name] = {
            "cursor": [cursor["parsedQuery"], plan["stage"], plan.get("limitAmount")],
            "stages": json.loads(json_util.dumps(_listed(reply["stages"]))),
            "run": json.loads(json_util.dumps(list(db.s.aggregate(pipeline)))),
        }
    return out


@pytest.mark.backlog_fixed("RM-200")
def test_explain_lists_hybrid_stages_as_mongod_desugars_them(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 listed `$score`, `$rankFusion` and `$scoreFusion` as
    the stages it desugars them into, a `$limit` behind `$score` in the
    cursor, and a score read as `{$meta: "score"}`; this server listed each
    stage as written."""
    got, want = _hybrid_listings(rust_mongo), _hybrid_listings(mongod)
    for name in HYBRID_LISTINGS:
        assert got[name] == want[name], name


FACETS: list[list] = [
    [{"$facet": {"f": [{"$rankFusion": {"input": {"pipelines": {"a": [{"$sort": {"x": -1}}]}}}}]}}],
    [
        {
            "$facet": {
                "f": [
                    {
                        "$rankFusion": {
                            "input": {
                                "pipelines": {
                                    "a": [{"$sort": {"x": -1}}],
                                    "b": [{"$sort": {"y": 1}}],
                                }
                            }
                        }
                    }
                ]
            }
        }
    ],
    [
        {
            "$facet": {
                "f": [
                    {
                        "$rankFusion": {
                            "input": {"pipelines": {"a": [{"$sort": {"x": -1}}]}},
                            "scoreDetails": True,
                        }
                    },
                    {"$project": {"s": {"$meta": "score"}, "d": {"$meta": "scoreDetails"}}},
                ],
                "g": [{"$count": "n"}],
            }
        }
    ],
    [
        {
            "$facet": {
                "f": [
                    {"$match": {}},
                    {"$rankFusion": {"input": {"pipelines": {"a": [{"$sort": {"x": -1}}]}}}},
                ]
            }
        }
    ],
    [
        {
            "$facet": {
                "f": [
                    {
                        "$scoreFusion": {
                            "input": {
                                "pipelines": {"a": [{"$score": {"score": "$x"}}]},
                                "normalization": "none",
                            }
                        }
                    }
                ]
            }
        }
    ],
    [{"$facet": {"f": [{"$project": {"s": {"$meta": "score"}}}]}}],
]


def _facets(client: Any) -> list:
    client.drop_database("rm200f")
    db = client["rm200f"]
    db.s.insert_many([{"_id": i, "x": i, "y": (i * 7) % 5} for i in range(6)])
    out = []
    for pipeline in FACETS:
        run = mongo_answer(lambda p=pipeline: list(db.s.aggregate(p)))
        listed = mongo_answer(
            lambda p=pipeline: db.command(
                "explain", {"aggregate": "s", "pipeline": p, "cursor": {}}, verbosity="queryPlanner"
            )["stages"]
        )
        if listed[0] == "ok":
            # The cursor under a `$facet` is RM-428's.
            listed = ("ok", _listed(listed[1]))
        out.append(json.loads(json_util.dumps([run, listed])))
    return out


@pytest.mark.backlog_fixed("RM-200")
def test_rank_fusion_runs_first_in_a_facet(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 ran a `$rankFusion` as the first stage of a `$facet`
    sub-pipeline, its second input over the collection, and listed it
    desugared; a fusion later in one is 10170100 and a `$scoreFusion` there
    is 40218. This server refused the first with 10170100."""
    assert _facets(rust_mongo) == _facets(mongod)
