"""Shared gauge plumbing: run any driver gauge against the Python *or* the Rust
SecantusDB server.

The gauges historically spawned ``python -m secantus`` as the daemon under test.
This module lets each gauge runner target either server, selected by the
``SECANTUS_GAUGE_SERVER`` environment variable (``python`` — the default — or
``rust``), mirroring the pymongo gauge's ``invoke validate --server rust``.

- **Python server**: ``python -m secantus <args>`` (unchanged).
- **Rust server**: the standalone ``secantusdb`` binary
  (``crates/secantusdb/target/{release,debug}/secantusdb`` or ``$SECANTUSDB_BIN``).
  The binary doesn't yet accept the Python server's tuning flags (``--log-level``
  / ``--noop-heartbeat-seconds`` / ``--cache-size`` / ``--session-max`` /
  ``--sync-on-commit`` — R7 tail), so [`for_server`] strips them.

Both servers advertise a ``secantus`` ``serverStatus`` marker, so each runner's
identity tripwire works against either.

Reports are written with a per-server suffix ([`report_suffix`]) — ``""`` for the
Python server, ``-rust-server`` for the Rust server — so a run against one server
never clobbers the other's report (matching the pymongo gauge's convention).
"""

from __future__ import annotations

import os
import pathlib

_REPO_ROOT = pathlib.Path(__file__).resolve().parent

# Python-server-only daemon flags the `secantusdb` binary doesn't accept yet;
# each takes a following value, dropped together when targeting the Rust server.
_PYTHON_ONLY_FLAGS = {
    "--log-level",
    "--noop-heartbeat-seconds",
    "--cache-size",
    "--session-max",
    "--sync-on-commit",
    "--oplog-retention-seconds",
}


def gauge_server() -> str:
    """The server under test: ``python`` (default) or ``rust``."""
    server = os.environ.get("SECANTUS_GAUGE_SERVER", "python")
    if server not in ("python", "rust"):
        raise SystemExit(f"SECANTUS_GAUGE_SERVER must be 'python' or 'rust', got {server!r}")
    return server


def report_suffix() -> str:
    """Report-filename suffix for the selected server (`""` / `-rust-server`)."""
    return "" if gauge_server() == "python" else "-rust-server"


def rust_binary() -> str:
    """Path to the standalone ``secantusdb`` binary, or a clear error."""
    env = os.environ.get("SECANTUSDB_BIN")
    if env and pathlib.Path(env).exists():
        return env
    for profile in ("release", "debug"):
        p = _REPO_ROOT / "crates" / "secantusdb" / "target" / profile / "secantusdb"
        if p.exists():
            return str(p)
    raise SystemExit(
        "secantusdb binary not built — run `cargo build --manifest-path "
        "crates/secantusdb/Cargo.toml` (needs WiredTiger; see tasks.py "
        "rust-binary-test), or set SECANTUSDB_BIN."
    )


def for_server(daemon_cmd: list[str]) -> list[str]:
    """Translate a ``python -m secantus <args>`` daemon command for the selected
    server. For the Python server the command is returned unchanged; for the Rust
    server the ``python -m secantus`` launcher is swapped for the ``secantusdb``
    binary and the Python-only tuning flags ([`_PYTHON_ONLY_FLAGS`], with their
    values) are dropped. The shared connection flags (``--host`` / ``--port`` /
    ``--storage-path`` / ``--auth``) carry over verbatim."""
    if gauge_server() != "rust":
        return daemon_cmd
    try:
        module_idx = daemon_cmd.index("secantus")
    except ValueError:
        raise SystemExit(
            f"for_server: expected a `python -m secantus ...` command, got {daemon_cmd!r}"
        )
    args = daemon_cmd[module_idx + 1 :]
    out = [rust_binary()]
    skip_value = False
    for arg in args:
        if skip_value:
            skip_value = False
            continue
        if arg in _PYTHON_ONLY_FLAGS:
            skip_value = True
            continue
        out.append(arg)
    return out
