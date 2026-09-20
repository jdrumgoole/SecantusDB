"""Simple protocol vs extended protocol, ours vs PostgreSQL.

`select 1` through psycopg uses the EXTENDED protocol (Parse/Bind/Describe/
Execute/Sync). An earlier "protocol floor" was measured with a SIMPLE query,
so the difference between them was being charged to statement processing.
"""

import pathlib
import statistics
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2]))

import psycopg  # noqa: E402
from bench.pg_statement_cost import RUST, _free_port, _wait  # noqa: E402


def bench(dsn, label):
    with psycopg.connect(dsn, autocommit=True) as c:
        pg = c.pgconn

        def simple(n=800):
            t0 = time.perf_counter()
            for _ in range(n):
                pg.exec_(b"select 1")
            return (time.perf_counter() - t0) / n * 1e6

        def extended(n=800):
            cur = c.cursor()
            t0 = time.perf_counter()
            for _ in range(n):
                cur.execute("select 1")
            return (time.perf_counter() - t0) / n * 1e6

        def ping(n=800):
            t0 = time.perf_counter()
            for _ in range(n):
                pg.exec_(b" ")
            return (time.perf_counter() - t0) / n * 1e6

        s = statistics.median([simple() for _ in range(3)])
        e = statistics.median([extended() for _ in range(3)])
        p = statistics.median([ping() for _ in range(3)])
        print(
            f"{label:16} ping={p:6.1f}  simple={s:6.1f}  "
            f"extended={e:6.1f}  (ext-simple={e - s:5.1f})"
        )


port = _free_port()
store = tempfile.mkdtemp(prefix="proto-")
d = subprocess.Popen(
    [str(RUST), store, f"127.0.0.1:{port}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
)
try:
    _wait("127.0.0.1", port)
    bench(f"host=127.0.0.1 port={port} dbname=postgres user=postgres", "secantusd-pg")
    bench("host=127.0.0.1 port=5432 dbname=postgres", "PostgreSQL 16")
finally:
    d.terminate()
    try:
        d.wait(timeout=10)
    except subprocess.TimeoutExpired:
        d.kill()
