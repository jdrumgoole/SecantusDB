"""Gap tests for what working RM-604 (a window-stage field of the wrong
type) found on 2026-11-17, comparing with mongod 8.2.11 (a one-member
replica set, macOS arm64)."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _empty_nested_specs(client: Any) -> dict:
    db = client.rm604e
    client.drop_database(db.name)
    db.c.insert_one({"_id": 1})

    def run(stage: dict) -> tuple:
        return mongo_answer(lambda: list(db.c.aggregate([stage])))

    return {
        "densify-range-empty": run({"$densify": {"range": {}}}),
        "fill-output-empty": run({"$fill": {"output": {}}}),
    }


@pytest.mark.backlog_fixed("RM-716")
def test_an_empty_densify_range_or_fill_output_is_refused_as_mongod_refuses_it(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 parses `$densify.range` as it reads it, so `range: {}`
    was "'$densify.range.bounds' is missing" before the missing `field`, and
    refuses `$fill` `output: {}` with 6050203; this server named the missing
    `field` for the first and answered BadValue (2), "not supported by the
    Rust server", for the second."""
    expected = {
        "densify-range-empty": (
            "err",
            40414,
            "BSON field '$densify.range.bounds' is missing but a required field",
        ),
        "fill-output-empty": (
            "err",
            6050203,
            "The output field in '$fill' must contain an object with at least one element",
        ),
    }
    assert _empty_nested_specs(mongod_rs) == expected
    assert _empty_nested_specs(rust_mongo) == expected
