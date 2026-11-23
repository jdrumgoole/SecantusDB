"""A gap found 2026-11-20 while adding mongod's `--bind_ip` spelling to
`secantusd-mdb`: mongod binds every address in a `--bind_ip` list; this
server bound one, and refused a list by name (fixed: a listener per
address).

Reference: mongod 8.2.11 with `--bind_ip 127.0.0.1,::1` and no `--ipv6`
binds 127.0.0.1, logs "Found no addresses for peer" for ::1, and serves.
"""

from __future__ import annotations

import pathlib

import pytest

from tests.backlog.conftest import rust_mongo_server


@pytest.mark.backlog_fixed("RM-729")
def test_a_bind_ip_list_serves_as_mongod_serves_it(tmp_path: pathlib.Path) -> None:
    """Started with a two-address `--bind_ip`, the server answers on
    127.0.0.1, as mongod does, and logs mongod's warning for ::1; it
    used to refuse the list and not start."""
    with rust_mongo_server(tmp_path / "store", "--bind_ip", "127.0.0.1,::1") as client:
        assert client.admin.command("ping")["ok"] == 1.0
        log = client.admin.command("getLog", "global")["log"]
        assert any('"id":23021' in line and "Found no addresses for peer" in line for line in log)
