"""Gap tests for what working RM-531 (`$documents` of a non-array) found on
2026-11-14, comparing with mongod 8.2.11 (a one-member replica set, macOS
arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

# ---------------------------------------- a $documents element's type --


def _element_not_a_document(client: Any) -> dict:
    client.drop_database("rm531e")
    db = client.rm531e
    db.o.insert_one({"_id": 1})
    return {
        "aggregate_1": mongo_answer(lambda: list(db.aggregate([{"$documents": [{"a": 1}, 2]}]))),
        "in_lookup": mongo_answer(
            lambda: list(
                db.o.aggregate(
                    [{"$lookup": {"pipeline": [{"$documents": [{"a": 1}, "s"]}], "as": "z"}}]
                )
            )
        ),
    }


@pytest.mark.backlog("RM-653")
def test_a_documents_element_that_is_not_a_document_is_40228(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses `$documents: [{a: 1}, 2]` with 40228 "Executor
    error during aggregate command on namespace: <db>.$cmd.aggregate ::
    caused by :: elements within the array passed to $documents must
    evaluate to an object, but resulting value was: 2. Type of resulting
    value: 'int'. Input document: {_tempDocumentsField: 2}" (the outer
    collection inside a `$lookup`). This server answers 2 "each $documents
    element must be an object"."""
    assert _element_not_a_document(rust_mongo) == _element_not_a_document(mongod_rs)


# ------------------------------------ a $documents expression's error --


def _expression_error(client: Any) -> dict:
    client.drop_database("rm531x")
    db = client.rm531x
    db.o.insert_one({"_id": 1})
    return {
        "argument": mongo_answer(lambda: list(db.aggregate([{"$documents": {"$divide": [1, 0]}}]))),
        "element": mongo_answer(
            lambda: list(db.aggregate([{"$documents": [{"x": {"$divide": [1, 0]}}]}]))
        ),
        "lookup_let": mongo_answer(
            lambda: list(
                db.o.aggregate(
                    [
                        {
                            "$lookup": {
                                "let": {"z": 0},
                                "pipeline": [{"$documents": {"$divide": [1, "$$z"]}}],
                                "as": "q",
                            }
                        }
                    ]
                )
            )
        ),
    }


@pytest.mark.backlog("RM-654")
def test_a_documents_expression_error_is_the_expressions(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 answers `$documents: {$divide: [1, 0]}` and
    `$documents: [{x: {$divide: [1, 0]}}]` with 2 "Failed to optimize
    pipeline :: caused by :: can't $divide by zero", and the same inside a
    `$lookup` reading a `let` variable under the executor prefix too. This
    server answers 2 "$documents expression not supported" to all three."""
    assert _expression_error(rust_mongo) == _expression_error(mongod_rs)


# ------------------------------------------ $$NOW inside a $unionWith --


def _now_in_union(client: Any) -> dict:
    client.drop_database("rm531n")
    db = client.rm531n
    db.o.insert_one({"_id": 1})
    is_date = {"$eq": [{"$type": "$t"}, "date"]}
    return {
        "in_documents": mongo_answer(
            lambda: list(
                db.o.aggregate(
                    [
                        {"$unionWith": {"pipeline": [{"$documents": [{"t": "$$NOW"}]}]}},
                        {"$project": {"d": is_date}},
                    ]
                )
            )
        ),
        "in_a_later_stage": mongo_answer(
            lambda: list(
                db.o.aggregate(
                    [
                        {
                            "$unionWith": {
                                "pipeline": [
                                    {"$documents": [{"a": 1}]},
                                    {"$set": {"t": "$$NOW"}},
                                ]
                            }
                        },
                        {"$project": {"d": is_date}},
                    ]
                )
            )
        ),
    }


@pytest.mark.backlog("RM-655")
def test_now_is_bound_in_a_union_with_pipeline(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 binds `$$NOW` inside a `$unionWith` pipeline, in a
    `$documents` and in a later stage alike: both return the outer document
    and one whose `t` is a date. This server answers 2 "$documents
    expression not supported" to the first and "not supported by the Rust
    server" to the second."""
    expected = ("ok", [{"_id": 1, "d": False}, {"d": True}])
    assert _now_in_union(mongod_rs) == {"in_documents": expected, "in_a_later_stage": expected}
    assert _now_in_union(rust_mongo) == _now_in_union(mongod_rs)


# ------------------------------------ the namespace a run-time error names --


def _run_time_error_namespace(client: Any) -> dict:
    client.drop_database("rm531s")
    db = client.rm531s
    db.o.insert_one({"_id": 1})
    db.c.insert_one({"_id": 2, "a": 3})
    bad = {"$set": {"x": {"$divide": ["$a", 0]}}}
    return {
        "after_documents": mongo_answer(
            lambda: list(db.aggregate([{"$documents": [{"a": 1}]}, bad]))
        ),
        "lookup_documents": mongo_answer(
            lambda: list(
                db.o.aggregate(
                    [{"$lookup": {"pipeline": [{"$documents": [{"a": 1}]}, bad], "as": "z"}}]
                )
            )
        ),
        "lookup_from": mongo_answer(
            lambda: list(db.o.aggregate([{"$lookup": {"from": "c", "pipeline": [bad], "as": "z"}}]))
        ),
        "union_with_coll": mongo_answer(
            lambda: list(db.o.aggregate([{"$unionWith": {"coll": "c", "pipeline": [bad]}}]))
        ),
    }


@pytest.mark.backlog("RM-656")
def test_a_run_time_error_names_the_commands_namespace(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 names the command's namespace in "Executor error during
    aggregate command on namespace: ... :: caused by :: can't $divide by
    zero" wherever the stage runs: `rm531s.$cmd.aggregate` after a
    `$documents`, `rm531s.o` inside a `$lookup` or `$unionWith` on `o`, with
    or without `from` / `coll`. This server names `rm531s.` after
    `$documents` and `rm531s.c` inside a stage reading `c`."""
    assert _run_time_error_namespace(rust_mongo) == _run_time_error_namespace(mongod_rs)
