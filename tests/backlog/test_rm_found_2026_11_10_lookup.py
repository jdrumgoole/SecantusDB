"""Gap tests for what working RM-413 (a `$lookup` without `from`) found on
2026-11-10, comparing with mongod 8.2.11 (a one-member replica set, macOS
arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

# ------------------------------------------------- $lookup spec checks --


def _lookup_spec_checks(client: Any) -> dict:
    client.drop_database("rm413s")
    db = client.rm413s
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 2}])

    def lookup(spec: dict) -> Any:
        return mongo_answer(lambda: list(db.c.aggregate([{"$lookup": spec}])))

    return {
        "let_not_a_document": lookup({"from": "c", "pipeline": [], "let": 5, "as": "z"}),
        "local_field_alone": lookup({"from": "c", "localField": "a", "as": "z"}),
        "foreign_field_alone": lookup({"from": "c", "foreignField": "a", "as": "z"}),
        "local_field_alone_with_pipeline": lookup(
            {"from": "c", "pipeline": [{"$match": {}}], "localField": "a", "as": "z"}
        ),
    }


@pytest.mark.backlog_fixed("RM-528")
def test_lookup_refuses_a_bad_let_and_a_lone_join_field(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses a `$lookup` whose `let` is not a document (14,
    "BSON field '$lookup.let' is the wrong type 'int', expected type
    'object'") and one with `localField` or `foreignField` but not both (9,
    "$lookup requires both or neither of 'localField' and 'foreignField' to
    be specified"). This server ran the first, and the lone field with a
    pipeline, and answered 2 to the lone field without one."""
    assert _lookup_spec_checks(rust_mongo) == _lookup_spec_checks(mongod_rs)


# ------------------------------------- an empty array as localField --


def _empty_local_array(client: Any) -> dict:
    client.drop_database("rm413e")
    db = client.rm413e
    db.c.insert_one({"_id": 1, "arr": []})
    db.f.insert_many(
        [
            {"_id": "missing"},
            {"_id": "null", "b": None},
            {"_id": "empty", "b": []},
            {"_id": "one", "b": 1},
            {"_id": "array_of_null", "b": [None]},
        ]
    )

    def joined(spec: dict) -> Any:
        return mongo_answer(
            lambda: sorted(
                d["_id"] for d in next(db.c.aggregate([{"$lookup": {**spec, "as": "z"}}]))["z"]
            )
        )

    simple = {"from": "f", "localField": "arr", "foreignField": "b"}
    answers = {
        "simple": joined(simple),
        "with_pipeline": joined({**simple, "pipeline": [{"$project": {"_id": 1}}]}),
        "collectionless": joined(
            {
                "localField": "arr",
                "foreignField": "b",
                "pipeline": [
                    {
                        "$documents": [
                            {"_id": "missing"},
                            {"_id": "null", "b": None},
                            {"_id": "empty", "b": []},
                            {"_id": "one", "b": 1},
                        ]
                    }
                ],
            }
        ),
    }
    # The simple form over an index on the foreign field probes the index.
    db.f.create_index("b")
    answers["simple_indexed"] = joined(simple)
    return answers


@pytest.mark.backlog_fixed("RM-529")
def test_an_empty_local_array_joins_what_null_joins(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 joins a `localField` that is an empty array as it joins
    null: to the foreign documents whose field is missing, null or `[null]`,
    in the simple form (with or without an index on the foreign field), with
    a pipeline and without `from`. This server joined it to none."""
    assert _empty_local_array(rust_mongo) == _empty_local_array(mongod_rs)


# ------------------------------------------- $documents in a $facet --


def _documents_in_facet(client: Any) -> dict:
    client.drop_database("rm413f")
    db = client.rm413f
    db.c.insert_one({"_id": 1})

    def facet(sub: list) -> Any:
        return mongo_answer(lambda: list(db.c.aggregate([{"$facet": {"f": sub}}])))

    docs = {"$documents": [{"b": 1}]}
    return {
        "directly": facet([docs]),
        "in_union_with": facet([{"$unionWith": {"pipeline": [docs]}}]),
        "in_lookup": facet([{"$lookup": {"pipeline": [docs], "as": "z"}}]),
    }


@pytest.mark.backlog_fixed("RM-530")
def test_documents_is_refused_in_a_facet(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses `$documents` in a `$facet` with 40600: "$documents
    is not allowed to be used within a $facet stage", or "$documents inside
    of $unionWith ..." / "... inside of $lookup ..." when a sub-pipeline in
    the facet holds it. This server answered 73 to the first and ran the
    other two."""
    assert _documents_in_facet(rust_mongo) == _documents_in_facet(mongod_rs)


# ---------------------------------------- $documents of a non-array --


def _documents_not_an_array(client: Any) -> dict:
    client.drop_database("rm413d")
    db = client.rm413d
    db.c.insert_one({"_id": 1})
    return {
        "aggregate_1": mongo_answer(lambda: list(db.aggregate([{"$documents": 5}]))),
        "in_lookup": mongo_answer(
            lambda: list(
                db.c.aggregate([{"$lookup": {"pipeline": [{"$documents": 5}], "as": "z"}}])
            )
        ),
    }


@pytest.mark.backlog_fixed("RM-531")
def test_documents_of_a_non_array_is_an_executor_error(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses `$documents: 5` with 5858203, "Executor error
    during aggregate command on namespace: <db>.$cmd.aggregate :: caused by
    :: an array is expected" (the outer collection's namespace inside a
    `$lookup`). This server answered 2 "$documents argument must evaluate to
    an array"."""
    assert _documents_not_an_array(rust_mongo) == _documents_not_an_array(mongod_rs)
