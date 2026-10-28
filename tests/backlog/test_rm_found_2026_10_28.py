"""Gap tests for the divergences found on 2026-10-28 while comparing a
fresh server's `config` database with mongod's (RM-366)."""

from __future__ import annotations

import time
from typing import Any

import pytest

# What a one-member mongod 8.2.11 set has in `config` within half a second
# of stepping up, every time (three fresh starts, 2026-10-28), beside
# `transactions` and `image_collection`, which this server creates too.
STEP_UP_COLLECTIONS = [
    "analyzeShardKeySplitPoints",
    "sampledQueries",
    "sampledQueriesDiff",
    "system.indexBuilds",
    "system.preimages",
]


def _step_up_collections(client: Any) -> list:
    """Which of `STEP_UP_COLLECTIONS` the server's `config` holds, once it
    has had the half second mongod takes to create them."""
    deadline = time.monotonic() + 5
    while True:
        names = set(client.config.list_collection_names())
        have = [n for n in STEP_UP_COLLECTIONS if n in names]
        if have == STEP_UP_COLLECTIONS or time.monotonic() > deadline:
            return have
        time.sleep(0.1)


@pytest.mark.backlog("RM-366")
def test_a_set_member_creates_its_step_up_collections(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 creates five more `config` collections as a replica-set
    member steps up: the index builds it resumes, the change stream
    pre-images, and the query sampler's three. This server creates only
    `transactions` and `image_collection`."""
    assert _step_up_collections(mongod_rs) == STEP_UP_COLLECTIONS  # the reference
    assert _step_up_collections(rust_mongo) == STEP_UP_COLLECTIONS
