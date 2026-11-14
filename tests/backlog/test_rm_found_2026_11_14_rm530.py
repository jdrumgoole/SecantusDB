"""Gap tests for what working RM-530 (`$documents` in a `$facet`) found on
2026-11-14, comparing with mongod 8.2.11 (a one-member replica set, macOS
arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

DOCS = {"$documents": [{"b": 1}]}


def _aggregate(client: Any, db_name: str, pipelines: dict) -> dict:
    client.drop_database(db_name)
    db = client[db_name]
    db.c.insert_one({"_id": 1, "a": 1})
    return {k: mongo_answer(lambda p=p: list(db.c.aggregate(p))) for k, p in pipelines.items()}


# ------------------------------------------ a $facet element's refusal --


def _facet_element(client: Any) -> dict:
    return _aggregate(
        client,
        "rm530e",
        {
            "int": [{"$facet": {"a": [{"$match": {}}], "b": [5]}}],
            "second_string": [{"$facet": {"a": [{"$match": {}}, "x"]}}],
            "empty_object": [{"$facet": {"a": [{}]}}],
        },
    )


@pytest.mark.backlog("RM-647")
def test_a_facet_element_refusal_names_the_element(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 names the element a `$facet` pipeline holds that is not
    a non-empty object (40171): "... b argument contained an element of type
    int: 0: 5". This server says "... b argument contained an invalid
    element"."""
    assert _facet_element(rust_mongo) == _facet_element(mongod_rs)


# ------------------------------- a $unionWith that reads no collection --


def _union_without_documents(client: Any) -> dict:
    union = {"$unionWith": {"pipeline": [{"$match": {}}]}}
    return _aggregate(
        client,
        "rm530u",
        {
            "in_facet": [{"$facet": {"a": [union]}}],
            "in_lookup": [{"$lookup": {"from": "c", "as": "z", "pipeline": [union]}}],
            "in_union": [{"$unionWith": {"coll": "c", "pipeline": [union]}}],
        },
    )


@pytest.mark.backlog("RM-648")
def test_a_nested_union_with_no_collection_must_make_its_documents(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses a `$unionWith` that names no `coll` and whose
    pipeline does not begin with `$documents` (9, "$unionWith stage without
    explicit collection must have a pipeline with $documents as first
    stage") wherever it stands. This server refuses it at the top level
    but, inside a `$facet`, `$lookup` or `$unionWith`, runs its pipeline
    over the documents of the collection it stands over and unions them in."""
    assert _union_without_documents(rust_mongo) == _union_without_documents(mongod_rs)


# ------------------------------ a $lookup spec check inside a pipeline --


def _nested_lookup_spec(client: Any) -> dict:
    bad_let = {"$lookup": {"from": "c", "pipeline": [], "let": 5, "as": "z"}}
    lone = {"$lookup": {"from": "c", "localField": "a", "as": "z"}}
    return _aggregate(
        client,
        "rm530l",
        {
            "let_in_facet": [{"$facet": {"a": [bad_let]}}],
            "lone_field_in_facet": [{"$facet": {"a": [lone]}}],
            "let_in_union": [{"$unionWith": {"coll": "c", "pipeline": [bad_let]}}],
        },
    )


@pytest.mark.backlog("RM-649")
def test_a_nested_lookup_spec_is_checked_as_it_is_parsed(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses a `$lookup` whose `let` is not a document (14)
    or that has `localField` without `foreignField` (9) wherever it stands,
    in a `$facet` or a `$unionWith` too. This server refuses them at the top
    level (RM-528) and runs them, or answers otherwise, inside one."""
    assert _nested_lookup_spec(rust_mongo) == _nested_lookup_spec(mongod_rs)


# ----------------------------------------- an empty $project, nested --


def _empty_projection(client: Any) -> dict:
    return _aggregate(
        client,
        "rm530p",
        {
            "top": [{"$project": {}}],
            "in_facet": [{"$facet": {"a": [{"$project": {}}]}}],
            "in_union": [{"$unionWith": {"coll": "c", "pipeline": [{"$project": {}}]}}],
        },
    )


@pytest.mark.backlog("RM-650")
def test_an_empty_projection_is_an_invalid_project(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses `{$project: {}}` with 51272, "Invalid $project
    :: caused by :: projection specification must have at least one field",
    at the top level and in a `$facet` or `$unionWith`. This server drops
    the "Invalid $project :: caused by ::" envelope at the top level and
    answers BadValue (2, "not supported by the Rust server") inside one."""
    assert _empty_projection(rust_mongo) == _empty_projection(mongod_rs)


# ---------------------------- $documents's own refusals in a sub-pipeline --


def _documents_refusals(client: Any) -> dict:
    return _aggregate(
        client,
        "rm530d",
        {
            "not_first_under_a_collection": [
                {"$unionWith": {"coll": "c", "pipeline": [{"$match": {}}, DOCS]}}
            ],
            "empty_spec": [{"$unionWith": {"pipeline": [{"$documents": {}}]}}],
        },
    )


@pytest.mark.backlog("RM-651")
def test_documents_refusals_in_a_sub_pipeline(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses a `$documents` that is not its pipeline's first
    stage with 40602, "$documents is only valid as the first stage in a
    pipeline", under a `$unionWith` that reads a collection too, and an
    empty `{$documents: {}}` with 51270, "Invalid $documents :: caused by
    :: Invalid empty sub-projection: _tempDocumentsField". This server
    answers the first 73 ("can only be run with {aggregate: 1}") and drops
    the second's "Invalid $documents :: caused by ::" envelope."""
    assert _documents_refusals(rust_mongo) == _documents_refusals(mongod_rs)


# --------------------- a $facet's refusal before a later stage's parse --


def _facet_before_later_stage(client: Any) -> dict:
    return _aggregate(
        client,
        "rm530o",
        {
            "then_empty_project": [{"$facet": {"a": [DOCS]}}, {"$project": {}}],
            "then_where": [{"$facet": {"a": [{"$out": "x"}]}}, {"$match": {"$where": "1"}}],
        },
    )


@pytest.mark.backlog("RM-652")
def test_a_facet_refusal_comes_before_a_later_stage(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 parses a pipeline stage by stage, so a `$facet` that
    holds a stage it refuses (40600) is refused before a later stage's
    parse error: `[{$facet: {a: [<$documents>]}}, {$project: {}}]` is
    40600. This server checks every stage's expressions and projections
    first, in passes over the whole pipeline, and answers the later
    stage's 51272 (or `$where`'s refusal)."""
    assert _facet_before_later_stage(rust_mongo) == _facet_before_later_stage(mongod_rs)
