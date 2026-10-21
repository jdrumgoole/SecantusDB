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

import pathlib
from typing import Any

import pytest
from bson import json_util

from tests.backlog.conftest import (  # noqa: F401  (fixtures)
    mongo_answer,
    mongod,
    mongod_rs,
    rust_mongo,
)

_CANONICAL = json_util.JSONOptions(json_mode=json_util.JSONMode.CANONICAL)


_HERE = pathlib.Path(__file__).parent


def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    # The hook sees every item in the session, not only this directory's:
    # marking them all would deselect the whole default suite.
    for item in items:
        if _HERE in pathlib.Path(str(item.fspath)).parents:
            item.add_marker(pytest.mark.mongod_live)


def canonical(value: Any) -> str:
    return json_util.dumps(value, json_options=_CANONICAL)


def same(ours: Any, theirs: Any) -> None:
    """Exact, type-aware equality, with both renderings in the message."""
    a, b = canonical(ours), canonical(theirs)
    assert a == b, f"\n  secantusd-rs: {a}\n  mongod:       {b}"
