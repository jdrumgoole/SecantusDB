"""Gap tests found working RM-634 (2026-11-23): a `{$meta: "sortKey"}` the
query layer does not take, with no `$sort` in front."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _answer(client: Any, pipeline: list) -> tuple:
    client.drop_database("rmfoundrm634")
    db = client.rmfoundrm634
    db.c.insert_many([{"_id": 1}, {"_id": 2}])
    return mongo_answer(lambda: list(db.c.aggregate(pipeline)))


@pytest.mark.backlog("RM-766")
@pytest.mark.parametrize(
    "pipeline",
    [
        [{"$limit": 5}, {"$project": {"t": {"$meta": "sortKey"}}}],
        [{"$project": {"_id": 1}}, {"$project": {"t": {"$meta": "sortKey"}}}],
        [{"$replaceWith": {"a": {"$meta": "sortKey"}}}],
    ],
    ids=["project-behind-a-limit", "second-project", "replace-with"],
)
def test_an_unpushed_sort_key_read_with_no_sort_is_empty(
    rust_mongo: Any, mongod: Any, pipeline: list
) -> None:
    """mongod 8.2.11 gives `[]` for `{$meta: "sortKey"}` with no `$sort`
    in a `$project` the query layer does not take, and in a
    `$replaceWith`, as it does in `$addFields`; this server refuses them
    as unsupported (code 2)."""
    assert _answer(rust_mongo, pipeline) == _answer(mongod, pipeline)
