"""Aggregation stage specs are rejected with mongod's codes, not a generic one.

The Rust engine signals "cannot do this" with a bare error carrying no code,
which the adapter reported as `BadValue` (2). A MALFORMED spec was therefore
indistinguishable from an unimplemented one, and every case here answered 2
where mongod has a specific code — so a driver matching on the code saw the
wrong error, and a caller could not tell a typo from an unsupported feature.

The Python server had most of these right already; `$fill`'s method was the
exception, raising our own phrasing under a generic 14.

Every code and message was probed against a live mongod 8.2.11 (2026-08-30).
These are validated at the COMMAND layer, before the engine runs — the same
place `$facet` already validates its spec.
"""

from __future__ import annotations

import tempfile
from collections.abc import Iterator

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture(scope="module")
def client() -> Iterator[MongoClient]:
    srv = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
    srv.start()
    cli = MongoClient(srv.address[0], srv.address[1], directConnection=True)
    cli["aggspec"].c.insert_many([{"_id": 1, "v": "s", "n": 1}, {"_id": 2, "v": "t", "n": 5}])
    try:
        yield cli
    finally:
        cli.close()
        srv.stop()


@pytest.mark.parametrize(
    ("stage", "code", "fragment"),
    [
        ({"$sample": {"size": -1}}, 28747, "size argument to $sample must be a positive integer"),
        ({"$sample": {"size": "x"}}, 28746, "size argument to $sample must be a number"),
        (
            {"$unwind": "nodollar"},
            28818,
            "path option to $unwind stage should be prefixed with a '$': nodollar",
        ),
        (
            {"$bucket": {"groupBy": "$n", "boundaries": [0], "default": "x"}},
            40192,
            "must have at least 2 values, but found 1 value(s).",
        ),
        (
            {"$densify": {"field": "n", "range": {"step": 0, "bounds": "full"}}},
            5733401,
            "must be a strictly positive numeric value",
        ),
        (
            {"$densify": {"field": "n", "range": {"step": -1, "bounds": "full"}}},
            5733401,
            "must be a strictly positive numeric value",
        ),
        (
            {"$fill": {"output": {"n": {"method": "bogus"}}}},
            6050202,
            "Method must be either locf or linear",
        ),
    ],
    ids=[
        "sample-negative",
        "sample-non-numeric",
        "unwind-unprefixed-path",
        "bucket-one-boundary",
        "densify-zero-step",
        "densify-negative-step",
        "fill-bad-method",
    ],
)
def test_malformed_spec_gets_mongods_code(
    client: MongoClient, stage: dict, code: int, fragment: str
) -> None:
    with pytest.raises(OperationFailure) as ei:
        list(client["aggspec"].c.aggregate([stage]))
    details = ei.value.details or {}
    assert details.get("code") == code, f"expected {code}, got {details.get('code')}"
    assert fragment in details.get("errmsg", "")


@pytest.mark.parametrize(
    "stage",
    [
        {"$sample": {"size": 1}},
        {"$unwind": "$v"},
        {"$bucket": {"groupBy": "$n", "boundaries": [0, 3, 9], "default": "other"}},
        {"$densify": {"field": "n", "range": {"step": 1, "bounds": "full"}}},
        {"$fill": {"sortBy": {"n": 1}, "output": {"v": {"method": "locf"}}}},
    ],
    ids=["sample", "unwind", "bucket", "densify", "fill"],
)
def test_valid_specs_still_run(client: MongoClient, stage: dict) -> None:
    """The validation must not reject the real thing."""
    list(client["aggspec"].c.aggregate([stage]))
