"""`configureFailPoint` is gated behind `enableTestCommands`, as mongod gates it.

W6 in `docs/security-reports/2026-08-10.md`: an unauthenticated client that can
reach the port could arm a server-wide `failCommand` with
`closeConnection: true`, dropping the socket of every subsequent operation on
*every* connection. mongod ships the same command but behind a startup
parameter, off by default.

Measured against mongod 8.2.11 (2026-09-28): started WITHOUT the parameter, it
answers `configureFailPoint` with `59 CommandNotFound :: no such command:
'configureFailPoint'` — not `Unauthorized`, not a no-op — and `getParameter`
reports `enableTestCommands: false`. So the gate makes the command *not exist*,
which is what these tests assert.

The split that matters: the standalone daemons default OFF (an operator exposes
them on a port), the embedded `SecantusDBServer` defaults ON (constructing one
in a test is its entire purpose).
"""

from __future__ import annotations

import inspect
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import gauge_common  # noqa: E402

from secantus import commands as commands_mod  # noqa: E402
from secantus.server import SecantusDBServer  # noqa: E402


def test_the_embedded_server_defaults_test_commands_on():
    """A test that constructs a server wants failpoints; that is the use case."""
    default = (
        inspect.signature(SecantusDBServer.__init__).parameters["enable_test_commands"].default
    )
    assert default is True


def test_the_embedded_server_can_turn_them_off(tmp_path):
    server = SecantusDBServer(
        port=0, storage_path=str(tmp_path / "off"), enable_test_commands=False
    )
    try:
        assert server.failpoints is None, "no registry is what makes the command vanish"
    finally:
        server.close() if hasattr(server, "close") else None


def test_a_registryless_context_reports_command_not_found(tmp_path):
    """The gate is the registry's absence, not a separate flag read at dispatch.

    Driven through a real server rather than a stub context, because a stub
    that happens to omit an unrelated field fails for the wrong reason — which
    is exactly what the first version of this test did.
    """
    server = SecantusDBServer(
        port=0, storage_path=str(tmp_path / "cnf"), enable_test_commands=False
    )
    ctx = commands_mod.CommandContext(server.storage, server.cursors, "admin")
    ctx.failpoints = server.failpoints
    reply = commands_mod.dispatch({"configureFailPoint": "failCommand"}, ctx)
    assert reply["ok"] == 0.0
    assert reply["code"] == 59
    assert reply["codeName"] == "CommandNotFound"
    # mongod's exact wording, so a driver that string-matches behaves the same.
    assert reply["errmsg"] == "no such command: 'configureFailPoint'"


def test_the_gauges_force_the_flag_on():
    """Thirteen daemon gauges need failpoints; one choke point turns it on.

    `tasks/driver-conformance-followups-plan.md` sized this as "every gauge task
    must pass the flag. Miss one and that gauge silently loses its failpoint
    coverage." Forcing it in `spawn_daemon` removes that whole class of mistake,
    so this pins the choke point rather than thirteen call sites.
    """
    cmd = ["python", "-m", "secantus", "--port", "0"]
    forced = gauge_common._force_test_commands(cmd)
    assert forced[-1] == "--enable-test-commands"
    # and adding it twice must not duplicate it
    assert gauge_common._force_test_commands(forced).count("--enable-test-commands") == 1


def test_the_two_servers_gate_the_same_command_set():
    """The Rust mirror is `is_test_only_command`; drift between them would mean
    one server exposing a command the other refuses."""
    rust_src = (
        Path(__file__).resolve().parents[1] / "crates" / "secantus-commands" / "src" / "lib.rs"
    ).read_text()
    for name in commands_mod._TEST_ONLY_COMMANDS:
        assert f'"{name}"' in rust_src.split("fn is_test_only_command")[1][:400], (
            f"{name} is gated on the Python server but not in the Rust is_test_only_command"
        )


@pytest.mark.parametrize("enabled", [True, False])
def test_get_parameter_reports_the_real_value(tmp_path, enabled):
    """It used to be a hardcoded `true`, which would now let a server that
    refuses the command claim to accept it — drivers gate their failpoint
    suites on this flag."""
    server = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / f"gp-{enabled}"),
        enable_test_commands=enabled,
    )
    try:
        ctx = commands_mod.CommandContext(server.storage, server.cursors, "admin")
        ctx.failpoints = server.failpoints
        reply = commands_mod.dispatch({"getParameter": 1, "enableTestCommands": 1}, ctx)
        assert reply["enableTestCommands"] is enabled
    finally:
        server.close() if hasattr(server, "close") else None
