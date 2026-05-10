"""Chaos monkey: repeatedly SIGKILL a SecantusDB and immediately restart it.

Spawns a SecantusDB server on a fixed port + persistent on-disk
WiredTiger storage, then runs a kill/restart loop in a background
thread. The server's storage path is preserved across kills so we
exercise the recovery path (oplog replay, WT journal, persisted
metadata) the same way a real crash would.

Pair with ``bench.load_writer`` (or ``invoke load``) pointed at the
same port. Run for ``--duration`` seconds, then stop the server and
print a post-run analysis: how many docs survived, where the gaps in
the ``n`` sequence are, and how those gaps correlate with kill events.

CLI:
    invoke chaos                                           # 180s default
    invoke chaos --duration 600 --min-interval 3 --max-interval 10
    invoke chaos --no-load                                 # server only

Why ``SIGKILL`` and not ``SIGTERM``: SIGTERM lets the server flush /
close cleanly, which doesn't exercise the recovery path. SIGKILL
matches a power-loss / OOM-killer scenario.
"""

from __future__ import annotations

import argparse
import contextlib
import datetime as _dt
import random
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from types import FrameType
from typing import Any

from pymongo import MongoClient

DEFAULT_DURATION = 180.0  # 3 minutes
DEFAULT_MIN_INTERVAL = 5.0
DEFAULT_MAX_INTERVAL = 15.0
DEFAULT_DB = "harness"
DEFAULT_COLLECTION = "inserts_8k"


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _wait_for_listener(host: str, port: int, *, timeout: float = 30.0) -> bool:
    """Poll TCP listen until the server is accepting connections."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return True
        except OSError:
            time.sleep(0.05)
    return False


def _ts() -> str:
    return _dt.datetime.now().strftime("%H:%M:%S.%f")[:-3]


def _spawn_server(port: int, storage_path: Path) -> subprocess.Popen[bytes]:
    """Start ``python -m secantus`` as a subprocess on ``port``."""
    return subprocess.Popen(
        [
            sys.executable, "-m", "secantus",
            "--host", "127.0.0.1",
            "--port", str(port),
            "--storage-path", str(storage_path),
            "--log-level", "WARNING",
        ],
        stdin=subprocess.DEVNULL,
        # Inherit stdout/stderr so the user sees server warnings inline
        # with kill/restart events.
    )


def _spawn_writer(
    uri: str, db: str, collection: str, *, batch_size: int = 1
) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [
            sys.executable, "-m", "bench.load_writer",
            "--uri", uri,
            "--db", db,
            "--collection", collection,
            "--drop",
            "--progress-every", "1000",
            "--batch-size", str(batch_size),
        ],
        stdin=subprocess.DEVNULL,
    )


def chaos_loop(
    state: dict[str, Any],
    *,
    storage_path: Path,
    port: int,
    min_interval: float,
    max_interval: float,
    stop_event: threading.Event,
    rng: random.Random,
) -> None:
    """Background kill/restart loop. Runs until ``stop_event`` is set."""
    while not stop_event.is_set():
        wait = rng.uniform(min_interval, max_interval)
        if stop_event.wait(wait):
            return
        proc = state["server_proc"]
        if proc.poll() is not None:
            # Server already exited (crash on its own?). Restart it.
            print(f"[{_ts()}] chaos: server already dead (rc={proc.returncode}), restarting", flush=True)
        else:
            print(f"[{_ts()}] chaos: SIGKILL pid={proc.pid}", flush=True)
            proc.kill()
            proc.wait()
        kill_at = time.monotonic()
        new_proc = _spawn_server(port, storage_path)
        state["server_proc"] = new_proc
        # Recovery on reopen replays the WT journal accumulated since
        # the last checkpoint (default cadence: every 60s). After tens
        # of thousands of writes the replay takes several seconds; bump
        # this generously so a slow restart doesn't false-positive as
        # "server failed to come up".
        if not _wait_for_listener("127.0.0.1", port, timeout=60.0):
            print(f"[{_ts()}] chaos: ERROR — server failed to come back up after kill", flush=True)
            stop_event.set()
            return
        downtime = time.monotonic() - kill_at
        state["kills"].append({
            "wall_time": _dt.datetime.now(_dt.timezone.utc),
            "downtime_seconds": downtime,
        })
        print(
            f"[{_ts()}] chaos: server back at pid={new_proc.pid} "
            f"(down {downtime*1000:.0f} ms, kill #{len(state['kills'])})",
            flush=True,
        )


def analyze_inserts(uri: str, db: str, collection: str) -> dict[str, Any]:
    """Inspect the surviving collection. Returns a stats dict."""
    client = MongoClient(uri, serverSelectionTimeoutMS=5000)
    try:
        coll = client[db][collection]
        # ``n`` is the attempt counter from the writer. Pull the projection
        # only — we don't need the 8 KB payload to count or find gaps.
        ns = sorted(d["n"] for d in coll.find({}, {"n": 1, "_id": 0}))
        total = len(ns)
        if total == 0:
            return {"total": 0, "max_n": 0, "gaps": [], "duplicates": 0}
        max_n = ns[-1]
        # Find gaps: any integer in 1..max_n that's missing from ns.
        present = set(ns)
        missing = sorted(set(range(1, max_n + 1)) - present)
        # Compact runs of consecutive missing ns into [start, end] ranges.
        gaps: list[tuple[int, int]] = []
        if missing:
            run_start = missing[0]
            run_end = missing[0]
            for x in missing[1:]:
                if x == run_end + 1:
                    run_end = x
                else:
                    gaps.append((run_start, run_end))
                    run_start = x
                    run_end = x
            gaps.append((run_start, run_end))
        duplicates = total - len(present)
        return {
            "total": total,
            "max_n": max_n,
            "missing_count": len(missing),
            "gaps": gaps,
            "duplicates": duplicates,
        }
    finally:
        client.close()


def run_chaos(
    *,
    duration: float,
    min_interval: float,
    max_interval: float,
    port: int | None,
    storage_path: Path | None,
    run_load: bool,
    db: str,
    collection: str,
    seed: int | None,
    batch_size: int = 1,
) -> int:
    rng = random.Random(seed)
    chosen_port = port or _free_port()
    if storage_path is None:
        # Persistent across kills, removed at the end. Real on-disk WT
        # so we exercise the journal/recovery path.
        storage_dir = Path(tempfile.mkdtemp(prefix="secantus-chaos-"))
        cleanup_storage = True
    else:
        storage_dir = storage_path
        storage_dir.mkdir(parents=True, exist_ok=True)
        cleanup_storage = False
    uri = f"mongodb://127.0.0.1:{chosen_port}/"
    print(
        f"[{_ts()}] chaos: starting "
        f"server=127.0.0.1:{chosen_port} storage={storage_dir} duration={duration:.0f}s "
        f"interval=[{min_interval:.1f},{max_interval:.1f}]s",
        flush=True,
    )

    server_proc = _spawn_server(chosen_port, storage_dir)
    if not _wait_for_listener("127.0.0.1", chosen_port, timeout=30.0):
        print(f"[{_ts()}] chaos: ERROR — initial server failed to start", flush=True)
        server_proc.kill()
        if cleanup_storage:
            shutil.rmtree(storage_dir, ignore_errors=True)
        return 2
    print(f"[{_ts()}] chaos: server up pid={server_proc.pid}", flush=True)

    state: dict[str, Any] = {"server_proc": server_proc, "kills": []}
    stop_event = threading.Event()

    def _on_signal(signum: int, _frame: FrameType | None) -> None:
        print(f"\n[{_ts()}] chaos: received signal {signum}, stopping", flush=True)
        stop_event.set()

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    chaos_thread = threading.Thread(
        target=chaos_loop,
        kwargs=dict(
            state=state, storage_path=storage_dir, port=chosen_port,
            min_interval=min_interval, max_interval=max_interval,
            stop_event=stop_event, rng=rng,
        ),
        name="chaos-monkey",
        daemon=True,
    )
    chaos_thread.start()

    writer_proc: subprocess.Popen[bytes] | None = None
    if run_load:
        writer_proc = _spawn_writer(uri, db, collection, batch_size=batch_size)
        print(
            f"[{_ts()}] chaos: writer up pid={writer_proc.pid} batch_size={batch_size}",
            flush=True,
        )

    # Main wait. Use Event.wait so signals can interrupt early.
    stop_event.wait(timeout=duration)
    stop_event.set()

    # Stop writer first so it doesn't keep banging on a server we're about to drop.
    if writer_proc is not None:
        with contextlib.suppress(ProcessLookupError):
            writer_proc.terminate()
        try:
            writer_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            writer_proc.kill()
            writer_proc.wait()

    chaos_thread.join(timeout=5)

    # Final clean shutdown of the server so the analysis client can connect.
    final_proc = state["server_proc"]
    if final_proc.poll() is None:
        with contextlib.suppress(ProcessLookupError):
            final_proc.terminate()
        try:
            final_proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            final_proc.kill()
            final_proc.wait()

    # Reopen for analysis (proves the persisted state is queryable after the run).
    print(f"\n[{_ts()}] chaos: reopening server for analysis", flush=True)
    analysis_proc = _spawn_server(chosen_port, storage_dir)
    try:
        if not _wait_for_listener("127.0.0.1", chosen_port, timeout=30.0):
            print(f"[{_ts()}] chaos: ERROR — analysis server failed to start", flush=True)
            return 3
        stats = analyze_inserts(uri, db, collection)
    finally:
        analysis_proc.terminate()
        with contextlib.suppress(subprocess.TimeoutExpired):
            analysis_proc.wait(timeout=10)
        if analysis_proc.poll() is None:
            analysis_proc.kill()
            analysis_proc.wait()

    print(f"\n=== chaos report ({duration:.0f}s wall-clock) ===")
    print(f"kills:           {len(state['kills'])}")
    if state["kills"]:
        downtimes = [k["downtime_seconds"] * 1000 for k in state["kills"]]
        print(
            f"downtime (ms):   min={min(downtimes):.0f}  "
            f"avg={sum(downtimes) / len(downtimes):.0f}  max={max(downtimes):.0f}"
        )
    print(f"docs persisted:  {stats['total']:,d}")
    print(f"max n attempted: {stats['max_n']:,d}")
    print(f"missing ns:      {stats['missing_count']:,d}  ({len(stats['gaps'])} gap{'s' if len(stats['gaps']) != 1 else ''})")
    print(f"duplicates:      {stats['duplicates']:,d}")
    if stats["gaps"]:
        # Show first few + last few gaps.
        head = stats["gaps"][:5]
        tail = stats["gaps"][-5:] if len(stats["gaps"]) > 10 else []
        print("first gaps:")
        for lo, hi in head:
            width = hi - lo + 1
            print(f"  n={lo:,d}..{hi:,d}  ({width:,d} missing)")
        if tail:
            print("  ...")
            for lo, hi in tail:
                width = hi - lo + 1
                print(f"  n={lo:,d}..{hi:,d}  ({width:,d} missing)")

    if cleanup_storage:
        shutil.rmtree(storage_dir, ignore_errors=True)
    return 0


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="chaos",
        description=(
            "Run SecantusDB under random SIGKILL/restart while a load "
            "writer hammers it. Reports persistence + gap stats at the end."
        ),
    )
    p.add_argument("--duration", type=float, default=DEFAULT_DURATION,
                   help=f"Total run time in seconds (default: {DEFAULT_DURATION:.0f}).")
    p.add_argument("--min-interval", type=float, default=DEFAULT_MIN_INTERVAL,
                   help=f"Minimum seconds between kills (default: {DEFAULT_MIN_INTERVAL}).")
    p.add_argument("--max-interval", type=float, default=DEFAULT_MAX_INTERVAL,
                   help=f"Maximum seconds between kills (default: {DEFAULT_MAX_INTERVAL}).")
    p.add_argument("--port", type=int, default=None,
                   help="Server port (default: auto-pick a free port).")
    p.add_argument("--storage-path", type=Path, default=None,
                   help="WiredTiger storage dir (default: tempdir, removed at end).")
    p.add_argument("--no-load", dest="run_load", action="store_false",
                   help="Don't auto-start the load_writer (chaos only).")
    p.add_argument("--db", default=DEFAULT_DB)
    p.add_argument("--collection", default=DEFAULT_COLLECTION)
    p.add_argument("--seed", type=int, default=None,
                   help="RNG seed for kill timing (default: random).")
    p.add_argument("--batch-size", type=int, default=1,
                   help="Documents per insert call in the writer (default: 1).")
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    return run_chaos(
        duration=args.duration,
        min_interval=args.min_interval,
        max_interval=args.max_interval,
        port=args.port,
        storage_path=args.storage_path,
        run_load=args.run_load,
        db=args.db,
        collection=args.collection,
        seed=args.seed,
        batch_size=max(1, args.batch_size),
    )


if __name__ == "__main__":
    raise SystemExit(main())
