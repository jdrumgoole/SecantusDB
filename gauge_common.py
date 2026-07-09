"""Shared gauge plumbing: run any driver gauge against the Python *or* the Rust
SecantusDB server.

The gauges historically spawned ``python -m secantus`` as the daemon under test.
This module lets each gauge runner target either server, selected by the
``SECANTUS_GAUGE_SERVER`` environment variable (``python`` — the default — or
``rust``), mirroring the pymongo gauge's ``invoke validate --server rust``.

- **Python server**: ``python -m secantus <args>`` (unchanged).
- **Rust server**: the standalone ``secantusd-rs`` binary
  (``crates/secantusdb/target/{release,debug}/secantusd-rs`` or ``$SECANTUSDB_BIN``).
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
import queue
import re
import subprocess
import threading
import time

_REPO_ROOT = pathlib.Path(__file__).resolve().parent

# Both servers print "listening on <host>:<port>" once bound:
#   rust:   "secantusd-rs listening on 127.0.0.1:59091"   (stdout)
#   python: "... secantus.server: secantus listening on 127.0.0.1:59092"  (stderr)
_LISTENING_RE = re.compile(r"listening on (\d{1,3}(?:\.\d{1,3}){3}):(\d+)")

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
        p = _REPO_ROOT / "crates" / "secantusdb" / "target" / profile / "secantusd-rs"
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
        ) from None
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


def _force_port_zero(daemon_cmd: list[str]) -> list[str]:
    """Return ``daemon_cmd`` with the ``--port`` value rewritten to ``0`` (added
    if absent), so the kernel assigns a free port the daemon then reports."""
    out = list(daemon_cmd)
    try:
        i = out.index("--port")
        out[i + 1] = "0"
    except (ValueError, IndexError):
        out += ["--port", "0"]
    return out


def _force_log_level_info(daemon_cmd: list[str]) -> list[str]:
    """Return ``daemon_cmd`` with ``--log-level`` set to ``INFO`` (added if
    absent).

    [`spawn_daemon`] learns the kernel-assigned port by waiting for the Python
    server's ``listening on <host>:<port>`` line — but the server logs that line
    at **INFO**, while every gauge passes ``--log-level WARNING`` to keep its
    output quiet. WARNING suppresses the readiness line, so ``spawn_daemon``
    would wait the full timeout (and, before the deadline-bounded read below,
    block indefinitely — a misconfigured gauge once hung CI for six hours). The
    server's per-request logging is at DEBUG, so forcing INFO surfaces only the
    single startup readiness line and adds no request noise. Python server only:
    the ``secantusdb`` binary prints its own listening line unconditionally and
    has ``--log-level`` stripped by [`for_server`]."""
    out = list(daemon_cmd)
    try:
        i = out.index("--log-level")
        out[i + 1] = "INFO"
    except (ValueError, IndexError):
        out += ["--log-level", "INFO"]
    return out


def spawn_daemon(
    daemon_cmd: list[str],
    *,
    label: str = "gauge",
    timeout: float = 30.0,
) -> tuple[subprocess.Popen, str, int]:
    """Spawn a SecantusDB daemon on a kernel-assigned port and return
    ``(process, host, port)`` once it has bound.

    This is the race-free replacement for "pick an ephemeral port, then spawn a
    daemon on it": picking and binding are two steps with a window in between, so
    when several gauges start at once two can grab the same just-freed port and a
    daemon ends up talking to the wrong server (or dies on ``EADDRINUSE``). Here
    the daemon binds ``--port 0`` itself and prints ``listening on <host>:<port>``;
    we read that line back, so there is no window and gauges parallelise safely.

    ``daemon_cmd`` is the usual ``python -m secantus ...`` form (it is translated
    for the Rust server via [`for_server`] and forced to ``--port 0``). stdout and
    stderr are merged and drained on a background thread so the daemon never
    blocks on a full pipe.
    """
    prepared = _force_port_zero(daemon_cmd)
    if gauge_server() == "python":
        # Ensure the readiness line the loop below greps for is actually emitted
        # (the gauges' --log-level WARNING would otherwise suppress it).
        prepared = _force_log_level_info(prepared)
    cmd = for_server(prepared)
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    assert proc.stdout is not None

    # Read the merged output on a background thread into a queue. A blocking
    # ``proc.stdout.readline()`` in the main loop never observes the deadline
    # when the daemon prints nothing (e.g. its readiness line is below the
    # configured log level), which turned a misconfigured gauge into a
    # multi-hour CI hang. The reader keeps draining after we've found the port,
    # so the daemon never blocks on a full pipe.
    lines: queue.Queue[str | None] = queue.Queue()

    def _read() -> None:
        try:
            for line in proc.stdout:  # type: ignore[union-attr]
                lines.put(line)
        except (ValueError, OSError):
            pass
        finally:
            lines.put(None)  # EOF sentinel

    threading.Thread(target=_read, daemon=True).start()

    host: str | None = None
    port: int | None = None
    deadline = time.monotonic() + timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        try:
            line = lines.get(timeout=remaining)
        except queue.Empty:
            break
        if line is None:  # daemon closed its output without binding
            if proc.poll() is not None:
                raise RuntimeError(f"{label}: daemon exited before binding a port")
            break
        m = _LISTENING_RE.search(line)
        if m:
            host, port = m.group(1), int(m.group(2))
            break

    if port is None:
        proc.terminate()
        raise RuntimeError(f"{label}: daemon did not report a listening port within {timeout}s")

    return proc, host, port
