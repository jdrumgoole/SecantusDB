"""Gap tests for what was found while working RM-332 (the sort refusals),
each asserting mongod's answer."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _meta_sort(client: Any) -> tuple:
    coll = client.rm332found.c
    coll.drop()
    coll.insert_many([{"_id": i} for i in range(5)])
    return mongo_answer(
        lambda: list(coll.aggregate([{"$sort": {"s": {"$meta": "randVal"}}}, {"$count": "n"}]))
    )


@pytest.mark.backlog("RM-423")
def test_a_sort_on_a_meta_key_runs(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.1 sorts by `{$meta: "randVal"}` in a `$sort` stage; the
    Rust server refuses the stage as unsupported."""
    assert _meta_sort(rust_mongo) == _meta_sort(mongod)
