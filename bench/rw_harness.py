"""Concurrent read/write validation harness.

Drives a configurable number of **independent worker processes** that
simultaneously read and write a shared collection on a SecantusDB
server, then proves that every acknowledged write is durable, correct,
and uncorrupted.

Two roles share this module:

* **orchestrator** (default) — brings up the server, spawns the
  workers, waits for them to finish, then runs a final full-collection
  validation sweep and prints a report.
* **worker** (``--role worker``) — one of the ``N`` independent
  processes. Inserts documents in its own disjoint ``_id`` range and,
  as it goes, reads a random sample of its prior writes back and
  verifies them (read-your-writes). Writes a JSON result file the
  orchestrator collects.

Server hosting is selectable with ``--server``:

* ``daemon`` (default) — spawn ``python -m secantus`` as a separate
  process, closest to a real deployment.
* ``embedded`` — run :class:`SecantusDBServer` in the orchestrator
  process; workers still connect over its TCP port.
* ``external`` — start nothing; point workers at an already-running
  ``--uri`` (e.g. a real ``mongod`` for differential testing).

**Highest database safety** is the default: writes use
``WriteConcern(w="majority", j=True)``, reads use
``ReadConcern("majority")`` against the primary, and the client enables
``retryWrites`` / ``retryReads``. SecantusDB is single-node, so
``w:"majority"`` is equivalent to ``w:1`` and ``readConcern`` is
accepted-and-ignored, but ``j:true`` is genuinely enforced (per-write
WT fsync). Pass ``--no-journal`` / ``--w 1`` to relax for comparison.

Validation has two layers:

1. **In-flight** — every read a worker performs recomputes the stored
   SHA-256 checksum over the document content and asserts it matches,
   catching torn or corrupted reads the moment they happen.
2. **Final sweep** — the orchestrator streams the *entire* collection
   (paginated cursor, never ``to_list()``), re-verifies every
   checksum, detects duplicate or missing ``_id``s, and confirms each
   worker's persisted document count equals the number of writes it
   reported as acknowledged.

Usage::

    invoke rw-harness                              # 4 workers, 1000 docs each, daemon
    uv run python -m bench.rw_harness --workers 8 --count 5000
    uv run python -m bench.rw_harness --server embedded --duration 30
    uv run python -m bench.rw_harness --server external --uri mongodb://127.0.0.1:27017/
"""

from __future__ import annotations

import argparse
import contextlib
import datetime as _dt
import hashlib
import json
import random
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from collections import defaultdict
from pathlib import Path
from types import FrameType
from typing import Any

from pymongo import MongoClient
from pymongo.errors import (
    AutoReconnect,
    ConnectionFailure,
    DuplicateKeyError,
    NetworkTimeout,
    ServerSelectionTimeoutError,
)
from pymongo.read_concern import ReadConcern
from pymongo.read_preferences import Primary
from pymongo.write_concern import WriteConcern

DEFAULT_WORKERS = 4
DEFAULT_COUNT = 1000
DEFAULT_DB = "rw_harness"
DEFAULT_COLLECTION = "rw_docs"
DEFAULT_PAYLOAD_BYTES = 256
DEFAULT_READ_EVERY = 10
DEFAULT_READ_BATCH = 5
DEFAULT_W = "majority"
SWEEP_BATCH = 1000

# Connection-class errors a worker treats as recoverable: the server
# went away, a reconnect is in flight, or a single op timed out. The
# write/read is counted as a failure and the worker keeps going, so the
# final report shows exactly how many ops fell in an outage window
# rather than aborting the whole run.
_RECOVERABLE_ERRORS = (
    AutoReconnect,
    ConnectionFailure,
    NetworkTimeout,
    ServerSelectionTimeoutError,
)


# --------------------------------------------------------------------------- #
# Document model + checksum
# --------------------------------------------------------------------------- #


def _doc_id(worker_id: int, seq: int) -> str:
    """Stable, disjoint ``_id`` for ``(worker_id, seq)``.

    Each worker owns the ``w{id}-*`` namespace, so workers never collide
    on ``_id`` even when sharing one collection — any DuplicateKeyError
    therefore means a ``retryWrites`` re-delivery of an already-committed
    insert, not a real clash.
    """
    return f"w{worker_id}-{seq}"


def _checksum(worker_id: int, seq: int, payload: str) -> str:
    return hashlib.sha256(f"{worker_id}|{seq}|{payload}".encode()).hexdigest()


def make_document(
    worker_id: int, seq: int, payload_bytes: int, rng: random.Random
) -> dict[str, Any]:
    """Build one self-validating document.

    ``payload`` is fresh random hex (so a stale/torn read is detectable),
    and ``checksum`` is a SHA-256 over ``worker_id|seq|payload`` — the
    reader recomputes it and compares, with no out-of-band state.
    """
    payload = rng.randbytes(payload_bytes).hex()
    seq_i = seq
    return {
        "_id": _doc_id(worker_id, seq_i),
        "w": worker_id,
        "s": seq_i,
        "payload": payload,
        "checksum": _checksum(worker_id, seq_i, payload),
        "ts": _dt.datetime.now(_dt.timezone.utc),
    }


def verify_document(doc: dict[str, Any]) -> bool:
    """Return True iff ``doc``'s stored checksum matches its content."""
    try:
        expected = _checksum(doc["w"], doc["s"], doc["payload"])
    except KeyError:
        return False
    return doc.get("checksum") == expected


# --------------------------------------------------------------------------- #
# Worker role
# --------------------------------------------------------------------------- #


def _safe_collection(
    client: MongoClient,
    db: str,
    collection: str,
    *,
    w: str | int,
    journal: bool,
    read_concern: str | None,
) -> Any:
    """Apply the configured durability/consistency options to a handle."""
    return client[db][collection].with_options(
        write_concern=WriteConcern(w=w, j=journal),
        read_concern=ReadConcern(level=read_concern) if read_concern else ReadConcern(),
        read_preference=Primary(),
    )


def worker_run(args: argparse.Namespace) -> dict[str, Any]:
    """Run one worker: interleave self-validating writes and read-backs."""
    client = MongoClient(
        args.uri,
        serverSelectionTimeoutMS=5000,
        retryWrites=True,
        retryReads=True,
    )
    coll = _safe_collection(
        client,
        args.db,
        args.collection,
        w=args.w,
        journal=args.journal,
        read_concern=args.read_concern,
    )
    rng = random.Random((args.seed << 16) ^ args.worker_id)

    stats: dict[str, Any] = {
        "worker_id": args.worker_id,
        "writes_ok": 0,
        "writes_failed": 0,
        "reads": 0,
        "read_mismatch": 0,
        "read_missing": 0,
        "read_errors": 0,
        "max_seq": 0,
    }
    written: list[int] = []

    stop_flag = [False]

    def _stop(signum: int, _frame: FrameType | None) -> None:
        stop_flag[0] = True

    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)

    deadline = time.monotonic() + args.duration if args.duration else None
    start = time.monotonic()
    seq = 0
    while not stop_flag[0]:
        if args.count and seq >= args.count:
            break
        if deadline is not None and time.monotonic() >= deadline:
            break
        seq += 1
        doc = make_document(args.worker_id, seq, args.payload_bytes, rng)
        try:
            coll.insert_one(doc)
            stats["writes_ok"] += 1
            written.append(seq)
        except DuplicateKeyError:
            # retryWrites re-issued an insert that already committed
            # before the ack was lost. On a durable backend the doc IS
            # there, so this is a success from the app's point of view.
            stats["writes_ok"] += 1
            written.append(seq)
        except _RECOVERABLE_ERRORS:
            stats["writes_failed"] += 1

        # Interleave read-backs: sample prior writes and validate them.
        if written and args.read_every and seq % args.read_every == 0:
            for _ in range(args.read_batch):
                rs = rng.choice(written)
                stats["reads"] += 1
                try:
                    got = coll.find_one({"_id": _doc_id(args.worker_id, rs)})
                except _RECOVERABLE_ERRORS:
                    stats["read_errors"] += 1
                    continue
                if got is None:
                    stats["read_missing"] += 1
                elif not verify_document(got):
                    stats["read_mismatch"] += 1

    stats["max_seq"] = seq
    stats["elapsed"] = time.monotonic() - start
    client.close()

    if args.result_file:
        Path(args.result_file).write_text(json.dumps(stats))
    return stats


# --------------------------------------------------------------------------- #
# Server lifecycle (orchestrator)
# --------------------------------------------------------------------------- #


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


def _spawn_daemon(
    port: int, storage_path: Path, *, sync_on_commit: bool
) -> subprocess.Popen[bytes]:
    argv = [
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
    if sync_on_commit:
        argv.append("--sync-on-commit")
    return subprocess.Popen(
        argv,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


# --------------------------------------------------------------------------- #
# Final validation sweep
# --------------------------------------------------------------------------- #


def validate_collection(
    coll: Any,
    *,
    expected_per_worker: dict[int, int],
) -> dict[str, Any]:
    """Stream the whole collection and check every invariant.

    Iterates a server-side cursor in ``SWEEP_BATCH`` batches — never
    ``to_list()`` — so the sweep is bounded in memory no matter how
    large the collection grew.
    """
    seen: set[str] = set()
    counts: dict[int, int] = defaultdict(int)
    report = {
        "total": 0,
        "bad_checksum": 0,
        "duplicate_id": 0,
        "malformed": 0,
    }
    cursor = coll.find({}, batch_size=SWEEP_BATCH)
    for doc in cursor:
        report["total"] += 1
        _id = doc.get("_id")
        if _id in seen:
            report["duplicate_id"] += 1
        else:
            seen.add(_id)
        if not verify_document(doc):
            report["bad_checksum"] += 1
        w = doc.get("w")
        if isinstance(w, int):
            counts[w] += 1
        else:
            report["malformed"] += 1

    # Per-worker reconciliation: persisted count must equal the number
    # of writes the worker reported as acknowledged.
    mismatches: list[tuple[int, int, int]] = []
    for wid, expected in sorted(expected_per_worker.items()):
        got = counts.get(wid, 0)
        if got != expected:
            mismatches.append((wid, expected, got))
    report["count_mismatches"] = mismatches
    return report


# --------------------------------------------------------------------------- #
# Orchestrator role
# --------------------------------------------------------------------------- #


def _spawn_workers(
    args: argparse.Namespace, uri: str, results_dir: Path
) -> list[subprocess.Popen[bytes]]:
    procs: list[subprocess.Popen[bytes]] = []
    for i in range(args.workers):
        result_file = results_dir / f"worker-{i}.json"
        argv = [
            sys.executable,
            "-m",
            "bench.rw_harness",
            "--role",
            "worker",
            "--worker-id",
            str(i),
            "--uri",
            uri,
            "--db",
            args.db,
            "--collection",
            args.collection,
            "--payload-bytes",
            str(args.payload_bytes),
            "--read-every",
            str(args.read_every),
            "--read-batch",
            str(args.read_batch),
            "--seed",
            str(args.seed),
            "--w",
            str(args.w),
            "--result-file",
            str(result_file),
        ]
        if not args.journal:
            argv.append("--no-journal")
        if args.read_concern:
            argv += ["--read-concern", args.read_concern]
        if args.count:
            argv += ["--count", str(args.count)]
        if args.duration:
            argv += ["--duration", str(args.duration)]
        procs.append(subprocess.Popen(argv, stdin=subprocess.DEVNULL))
    return procs


def _join_workers(procs: list[subprocess.Popen[bytes]], *, grace: float) -> None:
    """Wait for workers; SIGTERM then SIGKILL any that overstay ``grace``."""
    deadline = time.monotonic() + grace
    for p in procs:
        remaining = max(0.0, deadline - time.monotonic())
        try:
            p.wait(timeout=remaining)
        except subprocess.TimeoutExpired:
            break
    for p in procs:
        if p.poll() is None:
            with contextlib.suppress(ProcessLookupError):
                p.send_signal(signal.SIGTERM)
    for p in procs:
        try:
            p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()


def _collect_results(results_dir: Path, workers: int) -> list[dict[str, Any] | None]:
    out: list[dict[str, Any] | None] = []
    for i in range(workers):
        path = results_dir / f"worker-{i}.json"
        if path.exists():
            with contextlib.suppress(json.JSONDecodeError, OSError):
                out.append(json.loads(path.read_text()))
                continue
        out.append(None)
    return out


def _print_report(
    args: argparse.Namespace,
    results: list[dict[str, Any] | None],
    sweep: dict[str, Any],
) -> bool:
    """Print the per-worker + sweep report. Returns True iff all clean."""
    print("=" * 78)
    hdr = ("worker", "writes_ok", "wr_fail", "reads", "mismatch", "missing", "rd_err")
    print(f"{hdr[0]:<7}{hdr[1]:>11}{hdr[2]:>9}{hdr[3]:>9}{hdr[4]:>10}{hdr[5]:>9}{hdr[6]:>8}")
    print("-" * 78)
    tot = defaultdict(int)
    for i, r in enumerate(results):
        if r is None:
            print(f"{i:<7}{'(no result file — worker crashed?)':>62}")
            continue
        print(
            f"{r['worker_id']:<7}{r['writes_ok']:>11,}{r['writes_failed']:>9,}"
            f"{r['reads']:>9,}{r['read_mismatch']:>10,}{r['read_missing']:>9,}"
            f"{r['read_errors']:>8,}"
        )
        for k in (
            "writes_ok",
            "writes_failed",
            "reads",
            "read_mismatch",
            "read_missing",
            "read_errors",
        ):
            tot[k] += r[k]
    print("-" * 78)
    print(
        f"{'TOTAL':<7}{tot['writes_ok']:>11,}{tot['writes_failed']:>9,}"
        f"{tot['reads']:>9,}{tot['read_mismatch']:>10,}{tot['read_missing']:>9,}"
        f"{tot['read_errors']:>8,}"
    )
    print("=" * 78)

    print("\nfinal validation sweep:")
    print(f"  documents in collection : {sweep['total']:,}")
    print(f"  acknowledged writes      : {tot['writes_ok']:,}")
    print(f"  bad checksums            : {sweep['bad_checksum']:,}")
    print(f"  duplicate _ids           : {sweep['duplicate_id']:,}")
    print(f"  malformed docs           : {sweep['malformed']:,}")

    clean = True
    if sweep["bad_checksum"] or sweep["duplicate_id"] or sweep["malformed"]:
        clean = False
    if tot["read_mismatch"] or tot["read_missing"]:
        clean = False
    if sweep["count_mismatches"]:
        clean = False
        print("\n  PER-WORKER COUNT MISMATCHES (worker: expected != persisted):")
        for wid, expected, got in sweep["count_mismatches"]:
            print(f"    worker {wid}: acknowledged {expected:,} but found {got:,}")
    if sweep["total"] != tot["writes_ok"]:
        clean = False
        print(
            f"\n  TOTAL MISMATCH: {tot['writes_ok']:,} acknowledged writes "
            f"but {sweep['total']:,} documents persisted"
        )

    print()
    if clean:
        print("RESULT: PASS — every acknowledged write is durable, unique, and uncorrupted.")
    else:
        print("RESULT: FAIL — see mismatches above.")
    print()
    return clean


def orchestrate(args: argparse.Namespace) -> int:
    storage: Path | None = None
    daemon: subprocess.Popen[bytes] | None = None
    embedded: Any = None
    results_dir = Path(tempfile.mkdtemp(prefix="rw-harness-results-"))

    try:
        # --- bring up the server ---------------------------------------- #
        if args.server == "external":
            uri = args.uri
            if not _wait_listen(*_host_port(uri), timeout=10):
                print(f"ERROR: no server reachable at {uri}", file=sys.stderr)
                return 2
            print(f"server: external {uri}")
        elif args.server == "daemon":
            storage = (
                Path(args.storage_path)
                if args.storage_path
                else Path(tempfile.mkdtemp(prefix="rw-harness-data-"))
            )
            port = _free_port()
            daemon = _spawn_daemon(port, storage, sync_on_commit=args.sync_on_commit)
            if not _wait_listen("127.0.0.1", port, timeout=30):
                print("ERROR: daemon didn't come up", file=sys.stderr)
                return 2
            uri = f"mongodb://127.0.0.1:{port}/"
            print(f"server: daemon subprocess {uri} (storage: {storage})")
        else:  # embedded
            from secantus import SecantusDBServer

            storage = (
                Path(args.storage_path)
                if args.storage_path
                else Path(tempfile.mkdtemp(prefix="rw-harness-data-"))
            )
            embedded = SecantusDBServer(
                host="127.0.0.1",
                port=0,
                storage_path=str(storage),
                sync_on_commit=args.sync_on_commit,
            )
            embedded.start()
            uri = embedded.uri
            print(f"server: embedded in-process {uri} (storage: {storage})")

        # --- fresh collection ------------------------------------------- #
        admin = MongoClient(uri, serverSelectionTimeoutMS=5000)
        admin[args.db][args.collection].drop()
        admin.close()

        mode = f"--count {args.count}" if args.count else f"--duration {args.duration}s"
        print(
            f"workers: {args.workers}   mode: {mode}   shared collection: "
            f"{args.db}.{args.collection}\n"
            f"safety: w={args.w} j={args.journal} "
            f"readConcern={args.read_concern or '(default)'} retryWrites/Reads=on\n"
        )

        # --- run workers ------------------------------------------------ #
        t0 = time.monotonic()
        procs = _spawn_workers(args, uri, results_dir)
        grace = (
            (args.duration + 60.0)
            if args.duration
            else max(120.0, args.count * args.workers * 0.05)
        )
        _join_workers(procs, grace=grace)
        elapsed = time.monotonic() - t0
        print(f"all workers finished in {elapsed:.1f}s\n")

        # --- collect + validate ----------------------------------------- #
        results = _collect_results(results_dir, args.workers)
        expected = {r["worker_id"]: r["writes_ok"] for r in results if r is not None}
        client = MongoClient(uri, serverSelectionTimeoutMS=5000)
        coll = _safe_collection(
            client,
            args.db,
            args.collection,
            w=args.w,
            journal=args.journal,
            read_concern=args.read_concern,
        )
        sweep = validate_collection(coll, expected_per_worker=expected)
        client.close()

        clean = _print_report(args, results, sweep)
        return 0 if clean else 1
    finally:
        if embedded is not None:
            embedded.stop()
        if daemon is not None:
            daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
        shutil.rmtree(results_dir, ignore_errors=True)
        if storage is not None and not args.storage_path:
            shutil.rmtree(storage, ignore_errors=True)


def _host_port(uri: str) -> tuple[str, int]:
    """Extract (host, port) from a simple mongodb:// URI for a reachability poll."""
    rest = uri.split("://", 1)[-1].split("/", 1)[0]
    host, _, port = rest.partition(":")
    return host or "127.0.0.1", int(port or 27017)


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="rw_harness",
        description=(
            "Drive N independent reader/writer processes against SecantusDB "
            "and validate that every acknowledged write is durable and "
            "uncorrupted."
        ),
    )
    p.add_argument(
        "--role",
        choices=["orchestrator", "worker"],
        default="orchestrator",
        help="Internal: 'worker' is spawned by the orchestrator.",
    )
    p.add_argument(
        "--server",
        choices=["daemon", "embedded", "external"],
        default="daemon",
        help="How to host the server (default: daemon subprocess).",
    )
    p.add_argument(
        "--uri",
        default="mongodb://127.0.0.1:27018/",
        help="Server URI. Used directly with --server external; "
        "for daemon/embedded the orchestrator overrides it.",
    )
    p.add_argument(
        "--storage-path",
        default="",
        help="WiredTiger storage dir for daemon/embedded (default: tempdir, removed at end).",
    )
    p.add_argument(
        "--sync-on-commit",
        action="store_true",
        help="Start the server with --sync-on-commit (fsync every "
        "commit). Redundant with j:true but belt-and-suspenders.",
    )

    p.add_argument(
        "--workers",
        type=int,
        default=DEFAULT_WORKERS,
        help=f"Number of independent worker processes (default: {DEFAULT_WORKERS}).",
    )
    p.add_argument(
        "--count",
        type=int,
        default=DEFAULT_COUNT,
        help=f"Documents each worker writes, then stops (default: {DEFAULT_COUNT}). "
        "Mutually exclusive with --duration.",
    )
    p.add_argument(
        "--duration",
        type=float,
        default=0.0,
        help="Run each worker for this many seconds instead of a fixed count.",
    )

    p.add_argument("--db", default=DEFAULT_DB, help=f"Database (default: {DEFAULT_DB}).")
    p.add_argument(
        "--collection",
        default=DEFAULT_COLLECTION,
        help=f"Shared collection (default: {DEFAULT_COLLECTION}).",
    )
    p.add_argument(
        "--payload-bytes",
        type=int,
        default=DEFAULT_PAYLOAD_BYTES,
        help=f"Random payload size per doc (default: {DEFAULT_PAYLOAD_BYTES}).",
    )
    p.add_argument(
        "--read-every",
        type=int,
        default=DEFAULT_READ_EVERY,
        help=f"Read-back a sample every N writes (default: {DEFAULT_READ_EVERY}; 0 disables).",
    )
    p.add_argument(
        "--read-batch",
        type=int,
        default=DEFAULT_READ_BATCH,
        help=f"Documents validated per read-back (default: {DEFAULT_READ_BATCH}).",
    )
    p.add_argument("--seed", type=int, default=0, help="RNG seed base (default: 0).")

    p.add_argument(
        "--w",
        default=DEFAULT_W,
        help=f"writeConcern w (default: {DEFAULT_W!r}; int strings become ints).",
    )
    p.add_argument(
        "--journal",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="writeConcern j (default: --journal, i.e. j:true).",
    )
    p.add_argument(
        "--read-concern",
        default="majority",
        help="readConcern level (default: majority; empty for server default).",
    )

    p.add_argument("--worker-id", type=int, default=0, help="Internal: worker index.")
    p.add_argument("--result-file", default="", help="Internal: worker JSON result path.")
    return p.parse_args(argv)


def _normalise(args: argparse.Namespace) -> None:
    # ``--w 1`` should be the int 1, not the string "1"; "majority" stays a string.
    if isinstance(args.w, str) and args.w.isdigit():
        args.w = int(args.w)
    if args.read_concern == "":
        args.read_concern = None
    # --duration wins over --count when explicitly given.
    if args.duration:
        args.count = 0


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    _normalise(args)
    if args.role == "worker":
        worker_run(args)
        return 0
    if args.workers < 1:
        print("--workers must be >= 1", file=sys.stderr)
        return 2
    if not args.count and not args.duration:
        print("need --count or --duration", file=sys.stderr)
        return 2
    return orchestrate(args)


if __name__ == "__main__":
    raise SystemExit(main())
