"""Same workload as wt_pthread_bench.c, but driven through WiredTiger's
SWIG-generated Python bindings via Python threads.

Phase 3.1 gate criterion. We want the single-process scaling number for
the Python+SWIG path so we can compare it to the pure-C pthread path. If
C scales linearly to N=4 and Python+SWIG flatlines at N=2, the GIL
ceiling is identified at the SWIG bindings layer and the Cython rebind
plan from tasks/wt-bindings-plan.md is justified.

Usage:
    uv run python -m bench.wt_poc.wt_swig_bench <wt_home> <n_threads> <count>
"""

from __future__ import annotations

import argparse
import sys
import threading
import time

import wiredtiger

PAYLOAD = b"x" * 1024


def worker(conn, thread_id: int, count: int, errors: list[Exception]) -> None:
    try:
        session = conn.open_session(None)
        table = f"table:t{thread_id}"
        session.create(table, "key_format=q,value_format=u")
        cursor = session.open_cursor(table)
        for i in range(1, count + 1):
            cursor.set_key(i)
            cursor.set_value(PAYLOAD)
            cursor.insert()
        cursor.close()
        session.close()
    except Exception as exc:
        errors.append(exc)


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("home")
    p.add_argument("n_threads", type=int)
    p.add_argument("count", type=int)
    args = p.parse_args(argv)

    config = (
        "create,session_max=1000,cache_size=1G,"
        "log=(enabled=true,file_max=10MB),"
        "transaction_sync=(enabled=false,method=fsync)"
    )
    conn = wiredtiger.wiredtiger_open(args.home, config)

    threads = []
    errors: list[Exception] = []
    t0 = time.monotonic()
    for tid in range(args.n_threads):
        t = threading.Thread(target=worker, args=(conn, tid, args.count, errors))
        threads.append(t)
        t.start()
    for t in threads:
        t.join()
    t1 = time.monotonic()
    elapsed = t1 - t0

    total = args.n_threads * args.count
    rate = total / elapsed if elapsed > 0 else 0.0
    print(
        f"threads={args.n_threads} count={args.count} total={total} "
        f"elapsed={elapsed:.4f} rate={rate:.0f} errors={len(errors)}"
    )
    conn.close()
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
