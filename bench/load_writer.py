"""Continuous-insert load harness: write ≥8KB documents with a sequence counter.

Inserts a fixed-shape document containing an 8 KiB payload and a
monotonic ``n`` field (1, 2, 3, ...) representing the insert position.
Runs either ``--count`` times or continuously until interrupted (Ctrl-C
or SIGTERM).

Usage:
    uv run python -m bench.load_writer                         # continuous
    uv run python -m bench.load_writer --count 1000            # bounded
    uv run python -m bench.load_writer --uri mongodb://host:port/ --drop

Defaults to ``mongodb://127.0.0.1:27017/`` (a local SecantusDB or
mongod). Database and collection are ``loadtest.docs``.
"""

from __future__ import annotations

import argparse
import datetime as _dt
import signal
import sys
import time
from types import FrameType
from typing import Any

from pymongo import MongoClient
from pymongo.collection import Collection
from pymongo.errors import (
    AutoReconnect,
    ConnectionFailure,
    DuplicateKeyError,
    NetworkTimeout,
    ServerSelectionTimeoutError,
)

# Connection-class errors we treat as recoverable: server died, restart
# in progress, transient network blip. The writer logs the failure and
# moves on to ``n+1`` rather than aborting — gaps in ``n`` post-run map
# 1:1 to outage windows, which is the whole point of running this
# alongside chaos.
_RECOVERABLE_ERRORS = (
    AutoReconnect,
    ConnectionFailure,
    NetworkTimeout,
    ServerSelectionTimeoutError,
)

DEFAULT_URI = "mongodb://127.0.0.1:27017/"
DEFAULT_DB = "harness"
DEFAULT_COLLECTION = "inserts_8k"

# 8 KiB. The full BSON document is slightly larger (BSON envelope,
# field names, _id, n, ts), so each insert is comfortably ≥ 8 KiB on
# the wire — the spec is "at least 8k".
PAYLOAD_BYTES = 8 * 1024
PAYLOAD = "x" * PAYLOAD_BYTES


def make_document(n: int) -> dict[str, Any]:
    """Build the standard load-test document.

    Fields:
        n        — monotonic insert position (1, 2, 3, ...).
        payload  — 8192-byte ASCII string, identical across docs so the
                   harness measures throughput, not data generation.
        ts       — insert wall time, useful for tailing change streams
                   or correlating with server logs.
    """
    return {
        "n": n,
        "payload": PAYLOAD,
        "ts": _dt.datetime.now(_dt.timezone.utc),
    }


def run(
    coll: Collection,
    *,
    count: int | None,
    progress_every: int,
    stop_flag: list[bool],
    batch_size: int = 1,
) -> tuple[int, int]:
    """Insert documents in a tight loop. Returns ``(attempts, failures)``.

    ``count=None`` means run until ``stop_flag[0]`` is True. The flag
    is a one-element list so the signal handler can flip it without
    a global.

    ``batch_size > 1`` switches from per-doc ``insert_one`` to
    ``insert_many([...])``. The server-side ``Storage.insert`` already
    handles batches under a single lock, so wire-level batching is a
    significant throughput lever — at batch=100 inserts/s recover to
    several thousand even with WT logging on.

    Recoverable errors (server gone, reconnect in progress, network
    blip) are logged and counted as failures; ``n`` still advances so
    the post-run sequence shows gaps that line up with outages.
    """
    n = 0
    failures = 0
    last_report_n = 0
    last_report_t = time.monotonic()
    start = last_report_t
    while not stop_flag[0]:
        if count is not None and n >= count:
            break
        # Plan the next batch. Cap at the remaining count if --count is set.
        remaining = (count - n) if count is not None else batch_size
        chunk = max(1, min(batch_size, remaining))
        docs = [make_document(n + i + 1) for i in range(chunk)]
        attempted_lo = n + 1
        attempted_hi = n + chunk
        n += chunk
        try:
            if chunk == 1:
                coll.insert_one(docs[0])
            else:
                # ordered=False: a single dup-key in the middle doesn't
                # abort the rest. We don't currently expect dup keys
                # under chaos but it's the more forgiving default.
                coll.insert_many(docs, ordered=False)
        except DuplicateKeyError:
            # pymongo's default ``retryWrites=true`` retries an in-flight
            # insert when the connection drops mid-ack (e.g. server
            # SIGKILLed after the write committed but before the OK
            # reply landed). On a durable storage backend the original
            # write IS in the database, and the retry gets E11000.
            # That's a *succeeded* insert from the application's point
            # of view — count it as such, not as a failure.
            pass
        except _RECOVERABLE_ERRORS as exc:
            failures += chunk
            # Compact one-line warning so a flurry during a kill window
            # doesn't drown out progress lines. Includes the n range so
            # the gap is identifiable post-run.
            print(
                f"  ! n={attempted_lo}..{attempted_hi} insert failed: "
                f"{type(exc).__name__}: {exc}",
                flush=True,
            )
        if progress_every and n % progress_every == 0:
            now = time.monotonic()
            elapsed = now - last_report_t
            rate = (n - last_report_n) / elapsed if elapsed > 0 else 0.0
            print(
                f"  inserted n={n:>10,d}   "
                f"({rate:,.0f} attempts/s over last {progress_every:,}, "
                f"{failures:,d} failures so far)",
                flush=True,
            )
            last_report_n = n
            last_report_t = now
    total_elapsed = time.monotonic() - start
    avg_rate = n / total_elapsed if total_elapsed > 0 else 0.0
    succeeded = n - failures
    print(
        f"\nfinished: {n:,d} attempts in {total_elapsed:.2f}s "
        f"({avg_rate:,.0f} attempts/s avg) — "
        f"{succeeded:,d} succeeded, {failures:,d} failed",
        flush=True,
    )
    return n, failures


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        prog="load_writer",
        description=(
            "Insert standard 8 KiB documents with a sequence counter, "
            "either a fixed --count or continuously until Ctrl-C."
        ),
    )
    p.add_argument("--uri", default=DEFAULT_URI, help=f"MongoDB URI (default: {DEFAULT_URI})")
    p.add_argument("--db", default=DEFAULT_DB, help=f"Target database (default: {DEFAULT_DB})")
    p.add_argument(
        "--collection",
        default=DEFAULT_COLLECTION,
        help=f"Target collection (default: {DEFAULT_COLLECTION})",
    )
    p.add_argument(
        "--count",
        type=int,
        default=None,
        help="Number of documents to insert. Omit for continuous mode.",
    )
    p.add_argument(
        "--drop",
        action="store_true",
        help="Drop the target collection before inserting.",
    )
    p.add_argument(
        "--progress-every",
        type=int,
        default=1000,
        help="Print a progress line every N inserts (default: 1000; 0 to disable).",
    )
    p.add_argument(
        "--batch-size",
        type=int,
        default=1,
        help=(
            "Documents per insert call (default: 1 == per-doc insert_one). "
            "Larger batches use insert_many and dramatically increase throughput "
            "by amortising round-trip + per-transaction overhead."
        ),
    )
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    # ``serverSelectionTimeoutMS=2000``: pymongo's default is 30s, which
    # under chaos would mean every kill stalls the writer for half a
    # minute before the failure surfaces. 2s is plenty when the killed
    # server is being restarted ~immediately on the same port.
    client = MongoClient(args.uri, serverSelectionTimeoutMS=2000)
    coll = client[args.db][args.collection]

    if args.drop:
        coll.drop()
        print(f"dropped {args.db}.{args.collection}", flush=True)

    stop_flag = [False]

    def _stop(signum: int, _frame: FrameType | None) -> None:
        # First signal: request graceful stop. Second: propagate so the
        # interpreter exits even if pymongo is mid-network-call.
        if stop_flag[0]:
            print("\nforced exit", flush=True)
            sys.exit(130)
        print(f"\nreceived signal {signum}, finishing current insert and stopping...", flush=True)
        stop_flag[0] = True

    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)

    target_desc = f"{args.count:,d} docs" if args.count is not None else "continuous"
    print(
        f"writing to {args.uri} db={args.db} collection={args.collection} "
        f"({target_desc}, {PAYLOAD_BYTES:,d}-byte payload)",
        flush=True,
    )

    try:
        run(
            coll,
            count=args.count,
            progress_every=args.progress_every,
            stop_flag=stop_flag,
            batch_size=max(1, args.batch_size),
        )
    finally:
        client.close()

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
