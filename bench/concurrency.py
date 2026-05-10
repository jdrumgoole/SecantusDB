"""N-writer concurrency benchmark.

Spawns one SecantusDB server with on-disk WT storage, then runs a
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
import re
import shutil
import signal
import socket
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


def _spawn_server(port: int, storage_path: Path) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [
            sys.executable, "-m", "secantus",
            "--host", "127.0.0.1",
            "--port", str(port),
            "--storage-path", str(storage_path),
            "--log-level", "WARNING",
        ],
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
    """Spawn ``n`` writers, run for ``duration`` wall seconds, SIGTERM, return per-writer stats + elapsed."""
    procs: list[tuple[subprocess.Popen[bytes], Path]] = []
    log_paths: list[Path] = []
    try:
        for i in range(n):
            log_path = Path(tempfile.mkstemp(prefix=f"writer-{i}-", suffix=".log")[1])
            log_paths.append(log_path)
            log_f = log_path.open("w")
            collection = collection_prefix if shared_collection else f"{collection_prefix}{i}"
            argv = [
                sys.executable, "-m", "bench.load_writer",
                "--uri", uri,
                "--db", db,
                "--collection", collection,
                "--batch-size", str(batch),
                "--progress-every", "0",
            ]
            # Only the first writer drops; subsequent writers either share
            # (drop already done) or write to their own fresh collection.
            if i == 0:
                argv.append("--drop")
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
        for log_path in log_paths:
            stats.append(_parse_writer_log(log_path.read_text()))
        return stats, elapsed
    finally:
        for log_path in log_paths:
            log_path.unlink(missing_ok=True)


def run_concurrency_sweep(
    *,
    writers_list: list[int],
    duration: float,
    batch: int,
    shared_collection: bool,
) -> int:
    storage = Path(tempfile.mkdtemp(prefix="bench-concurrency-"))
    port = _free_port()
    server = _spawn_server(port, storage)
    try:
        if not _wait_listen("127.0.0.1", port, timeout=30):
            print("ERROR: server didn't come up", file=sys.stderr)
            return 2
        uri = f"mongodb://127.0.0.1:{port}/"
        coll_mode = "shared collection" if shared_collection else "per-writer collections"
        print(
            f"server: {uri}    duration: {duration:.0f}s/run    "
            f"batch: {batch}    mode: {coll_mode}\n"
        )

        results: list[tuple[int, int, float]] = []  # (n, total_succeeded, elapsed)
        for n in writers_list:
            print(f"running {n} writer{'s' if n != 1 else ''}...", flush=True)
            stats, elapsed = run_writers(
                uri,
                n=n,
                duration=duration,
                batch=batch,
                db=DEFAULT_DB,
                collection_prefix=DEFAULT_COLLECTION_PREFIX,
                shared_collection=shared_collection,
            )
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
            print(
                f"{n:<8} {total:>14,d} {elapsed:>9.2f}s {rate:>12,.0f} "
                f"{scaling:>11.2f}x"
            )
        print("=" * 72)
        if baseline_rate:
            print(
                f"\ninterpretation: scaling > 1.0x means concurrent writers "
                f"increase total throughput;"
                f"\n                scaling < 1.0x means contention is making "
                f"things worse than serial execution.\n"
            )
        return 0
    finally:
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait()
        shutil.rmtree(storage, ignore_errors=True)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="concurrency",
        description=(
            "Measure write-throughput scaling under N parallel writers. "
            "Phase 0 of the WT concurrency plan."
        ),
    )
    p.add_argument("--duration", type=float, default=DEFAULT_DURATION,
                   help=f"Wall-clock seconds per writer count (default: {DEFAULT_DURATION:.0f}).")
    p.add_argument("--batch-size", type=int, default=DEFAULT_BATCH,
                   help=f"Documents per insert call (default: {DEFAULT_BATCH}).")
    p.add_argument("--writers", default=DEFAULT_WRITERS,
                   help=f"Comma-separated writer counts (default: {DEFAULT_WRITERS}).")
    p.add_argument("--shared-collection", action="store_true",
                   help="All writers target the same collection (max contention).")
    return p.parse_args(argv)


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
    return run_concurrency_sweep(
        writers_list=writers_list,
        duration=args.duration,
        batch=max(1, args.batch_size),
        shared_collection=args.shared_collection,
    )


if __name__ == "__main__":
    raise SystemExit(main())
