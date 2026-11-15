"""Divergences found while keeping a projection's `{$meta: "sortKey"}`
fields under `returnKey` (a second RM-568 lane, 2026-11-15), each measured
against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.test_rm_found_rm470 import _answer


@pytest.mark.backlog("RM-706")
def test_return_key_with_a_sort_key_over_an_index_sort_explains_a_key_generator(
    rust_mongo: Any, mongod: Any
) -> None:
    """mongod 8.2.11 plans `find({y: {$gte: 0}}, {k: {$meta: "sortKey"}})
    .sort({y: 1}).returnKey()` over `{y: 1, x: 1}` as `RETURN_KEY <-
    SORT_KEY_GENERATOR <- IXSCAN`; this server's explain has no
    SORT_KEY_GENERATOR. The keys it returns match."""
    command = {
        "find": "c",
        "filter": {"y": {"$gte": 0}},
        "sort": {"y": 1},
        "projection": {"k": {"$meta": "sortKey"}},
        "returnKey": True,
    }
    plans = {
        server: [stage for stage, _ in _answer(client, command)[0]]
        for server, client in (("mongod", mongod), ("rust", rust_mongo))
    }
    assert plans["mongod"] == ["RETURN_KEY", "SORT_KEY_GENERATOR", "IXSCAN"]
    assert plans["rust"] == plans["mongod"]
