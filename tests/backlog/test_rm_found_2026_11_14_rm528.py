"""Gap tests for what working RM-528 (`$lookup`'s field types and join-field
pairing) found on 2026-11-14, comparing with mongod 8.2.11 (a one-member
replica set, macOS arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

# ------------------------------------- stage specs inside a sub-pipeline --


def _nested_stage_specs(client: Any) -> dict:
    client.drop_database("rm528n")
    db = client.rm528n
    db.create_collection("e")  # empty: nothing ever runs

    def agg(pipeline: list) -> Any:
        return mongo_answer(lambda: list(db.e.aggregate(pipeline)))

    return {
        "match_in_facet": agg([{"$facet": {"a": [{"$match": 5}]}}]),
        "lookup_let_in_facet": agg(
            [{"$facet": {"a": [{"$lookup": {"from": "c", "pipeline": [], "let": 5, "as": "z"}}]}}]
        ),
        "lone_field_in_lookup": agg(
            [
                {
                    "$lookup": {
                        "from": "c",
                        "as": "q",
                        "pipeline": [{"$lookup": {"from": "c", "localField": "a", "as": "z"}}],
                    }
                }
            ]
        ),
    }


@pytest.mark.backlog("RM-645")
def test_stage_specs_in_a_sub_pipeline_are_checked_as_it_parses(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses a malformed stage spec inside a `$facet` or a
    `$lookup` pipeline as the pipeline parses, over an empty collection:
    15959 for `$match: 5`, 14 for a `$lookup` `let` that is not a document,
    9 for a lone `localField`. This server answers 2 to the first and runs
    the other two."""
    assert _nested_stage_specs(rust_mongo) == _nested_stage_specs(mongod_rs)


# ------------------------------------- $lookup's paths and let names --


def _lookup_names(client: Any) -> dict:
    client.drop_database("rm528p")
    db = client.rm528p
    db.c.insert_one({"_id": 1, "a": 1})

    def lookup(spec: dict) -> Any:
        return mongo_answer(lambda: list(db.c.aggregate([{"$lookup": spec}])))

    return {
        "local_field_dollar": lookup(
            {"from": "c", "localField": "$a", "foreignField": "a", "as": "z"}
        ),
        "as_empty": lookup({"from": "c", "localField": "a", "foreignField": "a", "as": ""}),
        "let_bad_name": lookup({"from": "c", "let": {"X": 1}, "pipeline": [], "as": "z"}),
    }


@pytest.mark.backlog("RM-646")
def test_lookup_refuses_a_bad_path_and_a_bad_let_name(
    rust_mongo: Any,
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses a `localField` that starts with `$` (16410
    "... FieldPath field names may not start with '$', given '$a'."), an
    empty `as` (40352 "FieldPath cannot be constructed with empty string")
    and a `let` variable that is not a user variable name (9 "'X' starts
    with an invalid character for a user variable name"). This server runs
    all three."""
    assert _lookup_names(rust_mongo) == _lookup_names(mongod_rs)
