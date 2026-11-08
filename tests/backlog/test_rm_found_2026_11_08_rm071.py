"""Divergences found while explaining a `$group` mongod runs in SBE (RM-071,
2026-11-08), each measured against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest


def _sort_bytes(client: Any) -> Any:
    client.drop_database("rm071_found")
    col = client.rm071_found.c
    col.insert_many([{"a": i % 3, "b": i} for i in range(10)])
    pipeline = [{"$group": {"_id": "$a"}}, {"$sort": {"_id": 1}}]
    cmd = {"aggregate": "c", "pipeline": pipeline, "cursor": {}}
    e = col.database.command("explain", cmd, verbosity="executionStats")
    return e["stages"][1].get("totalDataSizeSortedBytesEstimate")


@pytest.mark.backlog("RM-435")
def test_a_sort_after_a_pushed_down_group_sizes_what_sbe_hands_it(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 sorts the three groups `[{$group: {_id: "$a"}}, {$sort:
    {_id: 1}}]` makes in SBE as 735 bytes; this server's `$sort` counts the
    same three documents as 351."""
    assert _sort_bytes(mongod) == 735
    assert _sort_bytes(rust_mongo) == _sort_bytes(mongod)
