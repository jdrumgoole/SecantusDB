"""Divergences found while writing mongod's startup lines (RM-305,
2026-11-10), each measured against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import only_on


def _arch(client: Any) -> tuple[str, str]:
    env = client.admin.command("buildInfo")["buildEnvironment"]
    return env["distarch"], env["target_arch"]


@pytest.mark.backlog("RM-500")
def test_build_info_names_the_architecture_as_mongod_does(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 on macOS arm64 names its architecture `arm64` in
    `buildInfo`'s `buildEnvironment` (`distarch`, `target_arch`) and in the
    `Build Info` startup line; this server gives Rust's name, `aarch64`.
    Elsewhere the names agree: Linux x86_64 mongod and Rust both say
    `x86_64` (CI, 2026-11-10), and Linux arm64's mongod says `aarch64`."""
    why = "macOS arm64 mongod says arm64 where Rust says aarch64"
    only_on("darwin", why=why)
    only_on("arm64", why=why)
    assert _arch(rust_mongo) == _arch(mongod_rs)
