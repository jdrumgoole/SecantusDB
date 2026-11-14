"""A gap found when snapshot PR #2735 went red (2026-11-14): the query
stats store's default budget is 1% of the machine's memory in whole
megabytes here, where mongod 8.2.11 takes 1% of it in bytes.

The reference is mongod, asked live (`mongod`); the Rust server is
`secantusd-rs` (`rust_mongo`). On macOS the memory is a whole number of
MiB, so the two agree there; Linux's `MemTotal` is not.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import only_on


@pytest.mark.backlog("RM-657")
def test_the_query_stats_budget_is_a_share_of_the_memory_in_bytes(
    rust_mongo: Any, mongod: Any
) -> None:
    """`metrics.queryStats.maxSizeBytes` with `internalQueryStatsCacheSize`
    at its default, 1%: mongod's `ProcessInfo::getMemSizeBytes` is Linux's
    `MemTotal` kB times 1024 (CI, 2026-11-14: 167584030); this server's is
    the memory in whole MiB (167583416)."""
    only_on("linux", why="macOS memory is a whole number of MiB, where the two agree")
    sizes = [
        c.admin.command("serverStatus")["metrics"]["queryStats"]["maxSizeBytes"]
        for c in (rust_mongo, mongod)
    ]
    assert sizes[0] == sizes[1], sizes
