"""The Rust MongoDB server against a live mongod 8.2, for surface no backlog
item covers.

`tests/backlog/` holds the comparisons a backlog item asked for; this holds
the ones written because nothing in CI compared the surface with mongod at
all (the coverage review of 2026-10-21: window functions, `$fill`, change
stream pre-images, retryable writes, and a tail of operators). Every test
here is marked `mongod_live`, which the default suite's addopts leave out:
it needs the `secantusd-rs` binary and a `mongod`, and the storage-engine
job in test.yml runs it with both. A missing reference is an error, never a
skip (the fixtures come from `tests/backlog/conftest.py`).

Compare with `same()`: it renders both answers as canonical extended JSON,
so an int32 that should be a double, or a field out of order, is a
difference rather than an equal Python value.
"""

from __future__ import annotations

import time
from collections.abc import Iterator
from typing import Any

import pymongo
import pytest
from bson import json_util

from tests.backlog.conftest import mongo_answer, mongod, rust_mongo  # noqa: F401  (fixtures)

_CANONICAL = json_util.JSONOptions(json_mode=json_util.JSONMode.CANONICAL)


def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    for item in items:
        item.add_marker(pytest.mark.mongod_live)


def canonical(value: Any) -> str:
    return json_util.dumps(value, json_options=_CANONICAL)


def same(ours: Any, theirs: Any) -> None:
    """Exact, type-aware equality, with both renderings in the message."""
    a, b = canonical(ours), canonical(theirs)
    assert a == b, f"\n  secantusd-rs: {a}\n  mongod:       {b}"


@pytest.fixture(scope="module")
def mongod_rs() -> Iterator[Any]:
    """A throwaway single-node replica set: the reference for change streams
    and retryable writes, which a standalone mongod refuses."""
    from tests.mongod_reference import MONGOD, _host_port, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod(extra_args=("--replSet", "live"))
    try:
        boot = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        host, port = _host_port(uri)
        boot.admin.command(
            {"replSetInitiate": {"_id": "live", "members": [{"_id": 0, "host": f"{host}:{port}"}]}}
        )
        deadline = time.monotonic() + 60
        while not boot.admin.command("hello").get("isWritablePrimary"):
            assert time.monotonic() < deadline, "the single-node set never became primary"
            time.sleep(0.2)
        boot.close()
        client = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        yield client
        client.close()
    finally:
        _stop_mongod(proc, dbpath)
