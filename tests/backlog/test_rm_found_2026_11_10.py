"""Divergences found while writing mongod's startup lines (RM-305,
2026-11-10), each measured against mongod 8.2.11."""

from __future__ import annotations

from typing import Any

import pytest


def _arch(client: Any) -> tuple[str, str]:
    env = client.admin.command("buildInfo")["buildEnvironment"]
    return env["distarch"], env["target_arch"]


@pytest.mark.backlog("RM-500")
def test_build_info_names_the_architecture_as_mongod_does(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 on macOS arm64 names its architecture `arm64` in
    `buildInfo`'s `buildEnvironment` (`distarch`, `target_arch`) and in the
    `Build Info` startup line; this server gives Rust's name, `aarch64`."""
    assert _arch(rust_mongo) == _arch(mongod_rs)
