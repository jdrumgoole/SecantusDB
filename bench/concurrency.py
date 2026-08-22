"""N-writer concurrency benchmark — Python server, Rust server, or mongod.

Spawns one server with on-disk WT storage (``--server python`` /
``rust`` / ``rust-async`` / ``mongod``, or ``all`` to sweep the four
back-to-back with a combined scaling table), then runs a
configurable list of writer counts (default ``1,2,4,8``) one after
another. For each count, ``N`` ``bench.load_writer`` processes write
``insert_many`` batches against their own collection for a fixed wall
clock duration, then are SIGTERMed. The harness parses each writer's
final ``finished: ... succeeded`` line and prints aggregate +
per-writer throughput plus the **scaling ratio** vs. the 1-writer
single baseline.

Each writer targets a distinct collection (``inserts_8k_w0``,
``inserts_8k_w1``, ...) so per-doc ``_id`` collisions don't muddy the
result. To exercise same-collection contention specifically, set
``--shared-collection``.

This is the Phase-0 instrument from ``tasks/wt-concurrency-plan.md``:
it gives us the regression-detector we need before changing the
locking model. Today's expected number is **0.35x at N=2** on a
single collection; Phase 2 has to push that above 1.5x.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

DEFAULT_DURATION = 30.0
DEFAULT_BATCH = 100
DEFAULT_WRITERS = "1,2,4"
DEFAULT_DB = "harness"
DEFAULT_COLLECTION_PREFIX = "inserts_8k_w"

# ``--server all`` sweep order. One full pass over every server per run,
# so multi-run medians interleave the servers (thermal / background
# drift lands on all of them, not just the last one measured).
ALL_SERVERS = ["python", "rust", "rust-async", "mongod"]

# Final summary line from ``bench/load_writer.py``:
#   ``finished: 80,000 attempts in 30.01s (2,665 attempts/s avg) — 80,000 succeeded, 0 failed``
_SUMMARY_RE = re.compile(
    r"finished:\s+(?P<attempts>[\d,]+)\s+attempts.*?(?P<succeeded>[\d,]+)\s+succeeded"
)


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_listen(host: str, port: int, *, timeout: float = 30.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.3):
                return True
        except OSError:
            time.sleep(0.05)
    return False


def _parse_writer_log(text: str) -> tuple[int, int] | None:
    m = _SUMMARY_RE.search(text)
    if not m:
        return None
    return (
        int(m["attempts"].replace(",", "")),
        int(m["succeeded"].replace(",", "")),
    )


def _rust_binary() -> str:
    """The standalone Rust daemon: $SECANTUSDB_BIN, the venv-staged copy
    (storage-engine wheel build), or the cargo target dir."""
    env = os.environ.get("SECANTUSDB_BIN")
    if env and Path(env).exists():
        return env
    cargo_target = Path(__file__).resolve().parent.parent / "crates" / "secantusdb" / "target"
    for cand in (
        Path(sys.executable).parent / "secantusd-rs",
        cargo_target / "release" / "secantusd-rs",
        cargo_target / "debug" / "secantusd-rs",
    ):
        if cand.exists():
            return str(cand)
    raise SystemExit(
        "secantusd-rs not found — build with "
        "SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev, "
        "or set SECANTUSDB_BIN."
    )


def _server_argv(server: str, port: int, storage_path: Path) -> list[str]:
    if server == "python":
        return [
            sys.executable,
            "-m",
            "secantus",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--storage-path",
            str(storage_path),
            "--log-level",
            "WARNING",
        ]
    if server in ("rust", "rust-async"):
        argv = [
            _rust_binary(),
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--storage-path",
            str(storage_path),
            "--log-level",
            "WARNING",
        ]
        if server == "rust-async":
            argv += ["--oplog-async", "--oplog-nonlogged"]
        return argv
    if server == "mongod":
        # SECANTUS_MONGOD_BIN pins a specific mongod, mirroring SECANTUSDB_BIN
        # for the Rust arm. `which("mongod")` on this box resolves to a 2024
        # symlink to mongodb-community@6.0 (6.0.16) while 8.3.4 is installed and
        # unlinked, so the default arm silently measures a two-year-old server.
        mongod = os.environ.get("SECANTUS_MONGOD_BIN") or shutil.which("mongod")
        if not mongod:
            raise SystemExit(
                "mongod not on PATH — install Community Server or skip --server mongod"
            )
        return [
            mongod,
            "--bind_ip",
            "127.0.0.1",
            "--port",
            str(port),
            "--dbpath",
            str(storage_path),
            "--quiet",
        ]
    raise SystemExit(f"unknown server {server!r}")


def _spawn_server(
    port: int, storage_path: Path, server: str = "python", server_log: Path | None = None
) -> subprocess.Popen[bytes]:
    if server_log is not None:
        out = server_log.open("ab")
        return subprocess.Popen(
            _server_argv(server, port, storage_path),
            stdin=subprocess.DEVNULL,
            stdout=out,
            stderr=subprocess.STDOUT,
        )
    return subprocess.Popen(
        _server_argv(server, port, storage_path),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def run_writers(
    uri: str,
    *,
    n: int,
    duration: float,
    batch: int,
    db: str,
    collection_prefix: str,
    shared_collection: bool,
) -> tuple[list[tuple[int, int] | None], float]:
    """Spawn ``n`` writers for ``duration`` wall seconds, SIGTERM; per-writer stats + elapsed."""
    procs: list[tuple[subprocess.Popen[bytes], Path]] = []
    log_paths: list[Path] = []
    # No drops anywhere near the measurement. The original scheme handed
    # writer 0 ``--drop`` (its drop starved behind the other writers' insert
    # stream, died summary-less on SIGTERM, and rows silently averaged a dead
    # writer — the 2026-08-11 sweep's 3.4x phantom "regression"); a harness
    # pre-drop fared no better, wedging 40+ minutes on the Rust server when
    # dropping a heavily-churned collection behind a WT eviction storm (that
    # server-side wedge is a real finding, filed in tasks/backlog.md). Each
    # row now writes to FRESH collection names — ``run_writers`` is handed a
    # per-row unique ``collection_prefix`` — so the window measures exactly N
    # writers inserting, nothing else.
    try:
        for i in range(n):
            log_path = Path(tempfile.mkstemp(prefix=f"writer-{i}-", suffix=".log")[1])
            log_paths.append(log_path)
            log_f = log_path.open("w")
            collection = collection_prefix if shared_collection else f"{collection_prefix}{i}"
            argv = [
                sys.executable,
                "-m",
                "bench.load_writer",
                "--uri",
                uri,
                "--db",
                db,
                "--collection",
                collection,
                "--batch-size",
                str(batch),
                "--progress-every",
                "0",
            ]
            p = subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=log_f,
                stderr=subprocess.STDOUT,
            )
            procs.append((p, log_path))

        t0 = time.monotonic()
        time.sleep(duration)
        elapsed = time.monotonic() - t0

        for p, _ in procs:
            with contextlib.suppress(ProcessLookupError):
                p.send_signal(signal.SIGTERM)
        for p, _ in procs:
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait()

        stats: list[tuple[int, int] | None] = []
        for i, log_path in enumerate(log_paths):
            text = log_path.read_text()
            parsed = _parse_writer_log(text)
            if parsed is None:
                # A writer that ends without its summary line is lost data; a
                # row built from N-1 writers presented as N is a corrupt
                # measurement (it once published a 3.4x phantom regression).
                # Refuse the run rather than averaging around the hole.
                tail = "\n".join(text.strip().splitlines()[-6:]) or "<empty log>"
                raise SystemExit(
                    f"writer {i} produced no summary — refusing to report a "
                    f"row measured with a missing writer; log tail:\n"
                    + "\n".join(f"    | {line}" for line in tail.splitlines())
                )
            stats.append(parsed)
        return stats, elapsed
    finally:
        for log_path in log_paths:
            log_path.unlink(missing_ok=True)


def _stop_server(server_proc: subprocess.Popen[bytes]) -> None:
    """Terminate a benchmark server, escalating to SIGKILL if it will not go."""
    server_proc.terminate()
    try:
        server_proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        server_proc.kill()
        server_proc.wait()


def run_concurrency_sweep(
    *,
    writers_list: list[int],
    duration: float,
    batch: int,
    shared_collection: bool,
    server: str = "python",
    server_log: Path | None = None,
) -> tuple[int, list[tuple[int, int, float]]]:
    coll_mode = "shared collection" if shared_collection else "per-writer collections"
    print(
        f"server: {server}    duration: {duration:.0f}s/run    "
        f"batch: {batch}    mode: {coll_mode}    store: fresh per row\n"
    )

    results: list[tuple[int, int, float]] = []  # (n, total_succeeded, elapsed)
    for n in writers_list:
        print(f"running {n} writer{'s' if n != 1 else ''}...", flush=True)
        # A FRESH store and server per row. Sharing one store across the
        # sweep made every row measure a different database: with 8,192-byte
        # documents, rows 1-4 leave tens of GB behind, so the 8-writer row
        # wrote into a store several times the size the 1-writer row saw.
        # That is a confound in a measurement whose entire purpose is to
        # isolate writer count, and it biases scaling downwards -- later
        # rows look worse partly because their tree is bigger.
        #
        # It was also a hard failure: on a 48GB droplet the accumulated
        # store exhausted the disk mid-sweep and WiredTiger took the
        # documented ENOSPC WT_PANIC ("the process must exit and restart"),
        # killing the row. Per-row stores bound peak usage to one row.
        row_storage = Path(tempfile.mkdtemp(prefix=f"bench-concurrency-n{n}-"))
        port = _free_port()
        server_proc = _spawn_server(port, row_storage, server, server_log)
        try:
            if not _wait_listen("127.0.0.1", port, timeout=30):
                print("ERROR: server didn't come up", file=sys.stderr)
                return 2, []
            uri = f"mongodb://127.0.0.1:{port}/"
            # Unique prefix per row: fresh collections, no drops (see
            # run_writers' note on why drops must never touch the window).
            stats, elapsed = run_writers(
                uri,
                n=n,
                duration=duration,
                batch=batch,
                db=DEFAULT_DB,
                collection_prefix=f"{DEFAULT_COLLECTION_PREFIX}n{n}_",
                shared_collection=shared_collection,
            )
        finally:
            _stop_server(server_proc)
            shutil.rmtree(row_storage, ignore_errors=True)

        total = sum(s[1] for s in stats if s)
        unparsed = sum(1 for s in stats if s is None)
        if unparsed:
            print(f"  WARN: {unparsed}/{n} writers produced no parseable summary", flush=True)
        results.append((n, total, elapsed))
        per_writer_rate = (total / n / elapsed) if elapsed > 0 else 0.0
        print(
            f"  total: {total:>10,d} docs in {elapsed:6.2f}s   "
            f"({per_writer_rate:>8,.0f} docs/s/writer)\n",
            flush=True,
        )

    # Summary
    baseline_rate = None
    if results and results[0][0] == 1:
        n0, total0, elapsed0 = results[0]
        baseline_rate = total0 / elapsed0 if elapsed0 > 0 else 0.0

    col1, col2, col3, col4, col5 = "writers", "total", "wall", "docs/s", "scaling"
    print("=" * 72)
    print(f"{col1:<8} {col2:>14} {col3:>10} {col4:>12} {col5:>12}")
    print("-" * 72)
    for n, total, elapsed in results:
        rate = total / elapsed if elapsed > 0 else 0.0
        scaling = (rate / baseline_rate) if baseline_rate else float("nan")
        print(f"{n:<8} {total:>14,d} {elapsed:>9.2f}s {rate:>12,.0f} {scaling:>11.2f}x")
    print("=" * 72)
    if baseline_rate:
        print(
            "\ninterpretation: scaling > 1.0x means concurrent writers "
            "increase total throughput;"
            "\n                scaling < 1.0x means contention is making "
            "things worse than serial execution.\n"
        )
    return 0, results


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="concurrency",
        description=(
            "Measure write-throughput scaling under N parallel writers. "
            "Phase 0 of the WT concurrency plan."
        ),
    )
    p.add_argument(
        "--duration",
        type=float,
        default=DEFAULT_DURATION,
        help=f"Wall-clock seconds per writer count (default: {DEFAULT_DURATION:.0f}).",
    )
    p.add_argument(
        "--batch-size",
        type=int,
        default=DEFAULT_BATCH,
        help=f"Documents per insert call (default: {DEFAULT_BATCH}).",
    )
    p.add_argument(
        "--writers",
        default=DEFAULT_WRITERS,
        help=f"Comma-separated writer counts (default: {DEFAULT_WRITERS}).",
    )
    p.add_argument(
        "--shared-collection",
        action="store_true",
        help="All writers target the same collection (max contention).",
    )
    p.add_argument(
        "--server-log",
        default="",
        help="Append the server's stdout/stderr to this file "
        "(default: discarded) — the harness's own diagnosis tool "
        "when writers report server errors.",
    )
    p.add_argument(
        "--server",
        default="python",
        choices=["python", "rust", "rust-async", "mongod", "all"],
        help="Which server to drive (default: python). "
        "'rust-async' is the Rust server's opt-in async + "
        "non-logged oplog stack. 'all' sweeps python, rust, "
        "rust-async, and mongod back-to-back and prints a "
        "combined table.",
    )
    p.add_argument(
        "--runs",
        type=int,
        default=1,
        help="Interleaved full sweeps; the reported rate per "
        "(server, writers) is the median across runs "
        "(default: 1).",
    )
    p.add_argument(
        "--json",
        default="",
        help="Write the median rates as JSON to this path — the "
        "input for bench.concurrency_chart, which refreshes "
        "the concurrency graphs on the website and in the docs.",
    )
    return p.parse_args(argv)


def assemble_results(
    *,
    writers_list: list[int],
    duration: float,
    batch: int,
    shared_collection: bool,
    runs: int,
    runs_rates: dict[str, list[list[float]]],
) -> dict:
    """Shape the per-run rates into the JSON payload ``concurrency_chart`` reads.

    ``runs_rates[server][i]`` is the list of docs/s observed for
    ``writers_list[i]`` across runs; the payload records both the raw
    per-run rates and their median.
    """
    return {
        "meta": {
            "duration": duration,
            "batch": batch,
            "writers": writers_list,
            "shared_collection": shared_collection,
            "runs": runs,
        },
        "servers": {
            server: {
                "runs_docs_per_sec": [[round(r, 1) for r in per_n] for per_n in rates],
                "docs_per_sec": [round(statistics.median(per_n), 1) for per_n in rates],
            }
            for server, rates in runs_rates.items()
        },
    }


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        writers_list = [int(x) for x in args.writers.split(",") if x.strip()]
    except ValueError:
        print(f"--writers must be a comma list of ints; got {args.writers!r}", file=sys.stderr)
        return 2
    if not writers_list:
        print("--writers cannot be empty", file=sys.stderr)
        return 2
    servers = list(ALL_SERVERS) if args.server == "all" else [args.server]
    runs = max(1, args.runs)
    runs_rates: dict[str, list[list[float]]] = {s: [[] for _ in writers_list] for s in servers}
    for run in range(runs):
        if runs > 1:
            print(f"### run {run + 1}/{runs}\n")
        for server in servers:
            rc, results = run_concurrency_sweep(
                writers_list=writers_list,
                duration=args.duration,
                batch=max(1, args.batch_size),
                shared_collection=args.shared_collection,
                server=server,
                server_log=Path(args.server_log) if args.server_log else None,
            )
            if rc != 0:
                return rc
            for i, (_n, total, elapsed) in enumerate(results):
                runs_rates[server][i].append(total / elapsed if elapsed > 0 else 0.0)
    medians = {s: [statistics.median(per_n) for per_n in runs_rates[s]] for s in servers}
    if len(servers) > 1 or runs > 1:
        label = "median docs/s" if runs > 1 else "docs/s"
        print("=" * 72)
        print(f"{'writers':<8}" + "".join(f"{s + ' ' + label:>24}" for s in servers))
        print("-" * 72)
        for i, n in enumerate(writers_list):
            row = f"{n:<8}"
            for s in servers:
                row += f"{medians[s][i]:>24,.0f}"
            print(row)
        print("=" * 72)
    if args.json:
        payload = assemble_results(
            writers_list=writers_list,
            duration=args.duration,
            batch=max(1, args.batch_size),
            shared_collection=args.shared_collection,
            runs=runs,
            runs_rates=runs_rates,
        )
        out = Path(args.json)
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
