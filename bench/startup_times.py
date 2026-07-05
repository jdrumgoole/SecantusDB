"""Compare cold-start startup latency of the three standalone servers.

Measures how long each of these single-node servers takes, from process
spawn to serving its first wire-protocol command, over a fresh (cold)
on-disk WiredTiger data directory:

- **mongod**            — real MongoDB, run as a standalone ``mongod``.
- **SecantusDB (py)**   — the pure-Python server (``python -m secantus``).
- **SecantusDB (rust)** — the pure-Rust server: the compiled ``secantusdb``
  binary from ``crates/secantusdb`` (no libpython, no Python interpreter in
  the process). The ``.venv/bin/secantusdb`` console-script is NOT used — it
  is a Python wrapper that launches the Python server.

All three are launched the same way — a throwaway process on a free port
with an empty data dir and ``--standalone`` topology — so the numbers are
apples-to-apples. Two timings are recorded per run:

- **listen**  — spawn → the port accepts a raw TCP connection.
- **ready**   — spawn → a fresh pymongo client completes ``admin.ping``
  (the real "ready to serve" moment, and the headline number).

Each server is measured ``--reps`` times over independent cold data dirs;
the report prints median / min / max / mean of both timings in ms.

Run it::

    uv run python -m bench.startup_times
    uv run python -m bench.startup_times --reps 10 --no-mongod --json

mongod is skipped with a note when it is not on PATH. The Rust server uses
the compiled ``secantusdb`` binary (release preferred, else debug;
auto-discovered under ``crates/`` and ``build/``, or set ``SECANTUSDB_BIN``);
build it with ``./inv rust-server-build`` or ``cargo build -p secantusdb``.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

from pymongo import MongoClient
from pymongo.errors import PyMongoError

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_REPS = 5


@dataclass(frozen=True)
class ServerSpec:
    """A server we can spawn as a standalone daemon on a chosen port."""

    key: str
    label: str
    # Build the argv given (port, data_dir). Returns None when unavailable.
    argv: object


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _is_native_binary(path: Path) -> bool:
    """True if ``path`` is a compiled executable, not a ``#!`` shebang script.

    Critical guard: ``.venv/bin/secantusdb`` is a Python console-script that
    launches the *Python* server, so it must never be picked for the Rust
    row — that would benchmark the Python server twice.
    """
    try:
        with open(path, "rb") as fh:
            return fh.read(2) != b"#!"
    except OSError:
        return False


def _rust_binary() -> str | None:
    """Locate the pure-Rust standalone ``secantusdb`` binary (no libpython).

    Prefers a release build, falls back to the debug build. Override with
    the ``SECANTUSDB_BIN`` environment variable. The ``.venv/bin/secantusdb``
    console-script is deliberately excluded — it is a Python wrapper around
    the Python server, not the Rust binary.
    """
    override = os.environ.get("SECANTUSDB_BIN")
    if override:
        return override if _is_native_binary(Path(override)) else None

    candidates = [
        REPO_ROOT / "crates" / "secantusdb" / "target" / "release" / "secantusdb",
        *sorted(REPO_ROOT.glob("build/*/secantusdb-target/release/secantusdb")),
        REPO_ROOT / "crates" / "secantusdb" / "target" / "debug" / "secantusdb",
    ]
    for candidate in candidates:
        if candidate.exists() and _is_native_binary(candidate):
            return str(candidate)
    return None


def _mongod_argv(port: int, data_dir: Path) -> list[str]:
    return [
        "mongod",
        "--bind_ip",
        "127.0.0.1",
        "--port",
        str(port),
        "--dbpath",
        str(data_dir),
        "--logpath",
        str(data_dir / "mongod.log"),
        "--noauth",
        "--quiet",
    ]


def _python_argv(port: int, data_dir: Path) -> list[str]:
    return [
        sys.executable,
        "-m",
        "secantus",
        "--host",
        "127.0.0.1",
        "--port",
        str(port),
        "--storage-path",
        str(data_dir),
        "--log-level",
        "WARNING",
        "--standalone",
    ]


def _rust_argv_factory(binary: str):
    # The pure-Rust binary has no --log-level flag (unlike python -m secantus).
    def build(port: int, data_dir: Path) -> list[str]:
        return [
            binary,
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--storage-path",
            str(data_dir),
            "--standalone",
        ]

    return build


def _tcp_open(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.2):
            return True
    except OSError:
        return False


def _ping_ok(port: int) -> bool:
    """One cheap, self-contained ``ping`` attempt with a short timeout."""
    client: MongoClient | None = None
    try:
        client = MongoClient(
            "127.0.0.1",
            port,
            directConnection=True,
            serverSelectionTimeoutMS=200,
            connectTimeoutMS=200,
        )
        client.admin.command("ping")
        return True
    except PyMongoError:
        return False
    finally:
        if client is not None:
            client.close()


@dataclass
class Timing:
    listen_s: float
    ready_s: float


def _measure_once(spec: ServerSpec, timeout: float) -> Timing:
    """Spawn the server on a fresh cold data dir; time listen + ready."""
    with tempfile.TemporaryDirectory(prefix=f"startup-{spec.key}-") as tmp:
        data_dir = Path(tmp)
        port = _free_port()
        argv = spec.argv(port, data_dir)

        t0 = time.monotonic()
        proc = subprocess.Popen(
            argv,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            stdin=subprocess.DEVNULL,
        )
        try:
            deadline = t0 + timeout
            listen_s = float("nan")
            ready_s = float("nan")

            # Phase 1: raw TCP listener up.
            while time.monotonic() < deadline:
                if proc.poll() is not None:
                    raise RuntimeError(
                        f"{spec.label} exited early (code {proc.returncode}) "
                        f"before accepting connections"
                    )
                if _tcp_open(port):
                    listen_s = time.monotonic() - t0
                    break
                time.sleep(0.002)
            else:
                raise RuntimeError(f"{spec.label} did not open port within {timeout}s")

            # Phase 2: wire protocol serving a command.
            while time.monotonic() < deadline:
                if _ping_ok(port):
                    ready_s = time.monotonic() - t0
                    break
                time.sleep(0.005)
            else:
                raise RuntimeError(f"{spec.label} did not serve ping within {timeout}s")

            return Timing(listen_s=listen_s, ready_s=ready_s)
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()


@dataclass
class Result:
    label: str
    listen_ms: list[float]
    ready_ms: list[float]


def _measure_server(spec: ServerSpec, reps: int, timeout: float, *, quiet: bool) -> Result:
    listen_ms: list[float] = []
    ready_ms: list[float] = []
    for i in range(reps):
        t = _measure_once(spec, timeout)
        listen_ms.append(t.listen_s * 1000.0)
        ready_ms.append(t.ready_s * 1000.0)
        if not quiet:
            print(
                f"  {spec.label:<18} rep {i + 1}/{reps}: "
                f"listen {t.listen_s * 1000.0:8.1f} ms   ready {t.ready_s * 1000.0:8.1f} ms",
                flush=True,
            )
    return Result(label=spec.label, listen_ms=listen_ms, ready_ms=ready_ms)


def _fmt_stats(samples: list[float]) -> str:
    return (
        f"{statistics.median(samples):8.1f}  "
        f"{min(samples):8.1f}  "
        f"{max(samples):8.1f}  "
        f"{statistics.fmean(samples):8.1f}"
    )


def _print_report(results: list[Result]) -> None:
    print()
    print("Cold startup latency (fresh on-disk WiredTiger, standalone topology)")
    print("=" * 78)
    header = f"{'server':<18} {'metric':<8} {'median':>8}  {'min':>8}  {'max':>8}  {'mean':>8}"
    print(header)
    print("-" * 78)
    # Sort fastest-ready first.
    for r in sorted(results, key=lambda r: statistics.median(r.ready_ms)):
        print(f"{r.label:<18} {'ready':<8} {_fmt_stats(r.ready_ms)}   (ms)")
        print(f"{'':<18} {'listen':<8} {_fmt_stats(r.listen_ms)}   (ms)")
    print("-" * 78)

    ready_medians = {r.label: statistics.median(r.ready_ms) for r in results}
    fastest_label = min(ready_medians, key=ready_medians.__getitem__)
    fastest = ready_medians[fastest_label]
    print(f"Fastest to serve: {fastest_label} ({fastest:.1f} ms median)")
    for label, med in sorted(ready_medians.items(), key=lambda kv: kv[1]):
        if label == fastest_label:
            continue
        print(f"  {label:<18} {med / fastest:5.2f}x slower to serve than {fastest_label}")
    print("=" * 78)


def _json_report(results: list[Result]) -> dict:
    return {
        "unit": "ms",
        "servers": [
            {
                "label": r.label,
                "ready": {
                    "median": statistics.median(r.ready_ms),
                    "min": min(r.ready_ms),
                    "max": max(r.ready_ms),
                    "mean": statistics.fmean(r.ready_ms),
                    "samples": r.ready_ms,
                },
                "listen": {
                    "median": statistics.median(r.listen_ms),
                    "min": min(r.listen_ms),
                    "max": max(r.listen_ms),
                    "mean": statistics.fmean(r.listen_ms),
                    "samples": r.listen_ms,
                },
            }
            for r in results
        ],
    }


def _build_specs(no_mongod: bool) -> list[ServerSpec]:
    specs: list[ServerSpec] = []

    if not no_mongod:
        if shutil.which("mongod") is not None:
            specs.append(ServerSpec("mongod", "mongod", _mongod_argv))
        else:
            print("mongod not on PATH; skipping the mongod comparison.", file=sys.stderr)

    specs.append(ServerSpec("python", "SecantusDB (py)", _python_argv))

    rust = _rust_binary()
    if rust is not None:
        specs.append(ServerSpec("rust", "SecantusDB (rust)", _rust_argv_factory(rust)))
    else:
        print(
            "secantusdb binary not found (looked in .venv/bin and PATH); "
            "skipping the Rust server. Build it with `./inv rust-server-build` "
            "or install the binary.",
            file=sys.stderr,
        )

    return specs


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="bench.startup_times",
        description="Compare cold-start startup latency of mongod, the SecantusDB "
        "Python server, and the SecantusDB Rust server (all standalone).",
    )
    parser.add_argument(
        "--reps",
        type=int,
        default=DEFAULT_REPS,
        help=f"Number of cold-start measurements per server (default: {DEFAULT_REPS}).",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=60.0,
        help="Per-run timeout in seconds waiting for the server to serve (default: 60).",
    )
    parser.add_argument(
        "--no-mongod",
        action="store_true",
        help="Skip the mongod comparison (SecantusDB servers only).",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable JSON instead of the formatted table.",
    )
    args = parser.parse_args(argv)

    if args.reps < 1:
        parser.error("--reps must be >= 1")

    specs = _build_specs(args.no_mongod)
    if not specs:
        print("No servers available to measure.", file=sys.stderr)
        return 1

    results: list[Result] = []
    for spec in specs:
        if not args.json:
            print(f"Measuring {spec.label} ({args.reps} cold starts)...", flush=True)
        results.append(_measure_server(spec, args.reps, args.timeout, quiet=args.json))

    if args.json:
        print(json.dumps(_json_report(results), indent=2))
    else:
        _print_report(results)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
