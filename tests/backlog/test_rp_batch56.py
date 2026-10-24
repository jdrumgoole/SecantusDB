"""Batch 56 of the Rust PG server (2026-10-24): the regression tests of
RP-100 (decided by Joe, 2026-10-22: stream COPY, and a progress slot for
every command): `pg_stat_progress_copy` read from another session while a
COPY FROM runs, its rows loaded as their data arrives -- a row split
between two CopyData messages, a CSV field holding a newline, a binary
tuple split mid-field, the data after a `\\.` line -- and the shapes of the
other progress views.

Each test runs the same code against a private, freshly initdb'd
PostgreSQL 15.19 cluster and this server; the literals are 15.19's answers,
measured by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch56.py "host=127.0.0.1 port=5499 user=postgres"
"""

from __future__ import annotations

import struct
import sys
import time
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_PROGRESS = (
    "select command, type, bytes_processed, bytes_total, tuples_processed, tuples_excluded, "
    "relid::regclass::text, datname::text, pid = %s from pg_stat_progress_copy"
)


def copy_progress(dsn: str) -> list[Any]:
    """What another session reads of a COPY FROM as its data arrives."""
    out: list[Any] = []
    with (
        psycopg.connect(dsn, autocommit=True) as a,
        psycopg.connect(dsn, autocommit=True) as b,
    ):
        a.execute("create table rp56_c (x int, t text)")
        try:
            pid = a.execute("select pg_backend_pid()").fetchone()[0]
            with a.cursor().copy("copy rp56_c from stdin") as cp:
                # The third row is split between two messages.
                cp.write("1\tone\n2\ttwo\n3\tth")
                time.sleep(0.3)
                out.append(b.execute(_PROGRESS, (pid,)).fetchall())
                cp.write("ree\n")
                time.sleep(0.3)
                out.append(b.execute(_PROGRESS, (pid,)).fetchall())
            out.append(b.execute(_PROGRESS, (pid,)).fetchall())
            with a.cursor().copy("copy rp56_c (x) from stdin with (format csv, header)") as cp:
                cp.write("x\n7\n")
                time.sleep(0.3)
                out.append(b.execute(_PROGRESS, (pid,)).fetchall())
            out.append(a.execute("select x, t from rp56_c order by x").fetchall())
        finally:
            a.execute("drop table rp56_c")
    return out


COPY_PROGRESS = [
    [("COPY FROM", "PIPE", 16, 0, 2, 0, "rp56_c", "postgres", True)],
    [("COPY FROM", "PIPE", 20, 0, 3, 0, "rp56_c", "postgres", True)],
    [],
    [("COPY FROM", "PIPE", 4, 0, 1, 0, "rp56_c", "postgres", True)],
    [(1, "one"), (2, "two"), (3, "three"), (7, None)],
]


def split_rows(dsn: str) -> list[Any]:
    """Rows whose data is split across CopyData messages wherever a client
    happens to split it, and what follows a `\\.` line."""
    out: list[Any] = []
    with psycopg.connect(dsn, autocommit=True) as a:
        a.execute("create table rp56_s (x int, t text)")
        try:
            with a.cursor().copy("copy rp56_s from stdin with (format csv)") as cp:
                cp.write('1,"a\nb"\n2,"c')
                cp.write('""d"\n3,e\n')
            with a.cursor().copy("copy rp56_s from stdin") as cp:
                cp.write("4\tf\n\\.\n5\tg\n")
            header = b"PGCOPY\n\xff\r\n\x00" + struct.pack("!ii", 0, 0)
            tuple_ = struct.pack("!hi", 2, 4) + struct.pack("!i", 6) + struct.pack("!i", 3) + b"hij"
            data = header + tuple_ + struct.pack("!h", -1)
            with a.cursor().copy("copy rp56_s from stdin with (format binary)") as cp:
                cp.write(data[:25])
                cp.write(data[25:])
            out.append(a.execute("select x, t from rp56_s order by x").fetchall())
        finally:
            a.execute("drop table rp56_s")
    return out


SPLIT_ROWS = [[(1, "a\nb"), (2, 'c"d'), (3, "e"), (4, "f"), (6, "hij")]]


def progress_view_shapes(dsn: str) -> list[Any]:
    """Every progress view answers, with PostgreSQL's columns, empty while
    no backend runs its command."""
    out: list[Any] = []
    with psycopg.connect(dsn, autocommit=True) as a:
        for v in ("vacuum", "analyze", "cluster", "create_index", "basebackup", "copy"):
            cur = a.execute(f"select * from pg_stat_progress_{v}")
            out.append((v, [c.name for c in cur.description], cur.fetchall()))
    return out


PROGRESS_VIEW_SHAPES = [
    (
        "vacuum",
        [
            "pid",
            "datid",
            "datname",
            "relid",
            "phase",
            "heap_blks_total",
            "heap_blks_scanned",
            "heap_blks_vacuumed",
            "index_vacuum_count",
            "max_dead_tuples",
            "num_dead_tuples",
        ],
        [],
    ),
    (
        "analyze",
        [
            "pid",
            "datid",
            "datname",
            "relid",
            "phase",
            "sample_blks_total",
            "sample_blks_scanned",
            "ext_stats_total",
            "ext_stats_computed",
            "child_tables_total",
            "child_tables_done",
            "current_child_table_relid",
        ],
        [],
    ),
    (
        "cluster",
        [
            "pid",
            "datid",
            "datname",
            "relid",
            "command",
            "phase",
            "cluster_index_relid",
            "heap_tuples_scanned",
            "heap_tuples_written",
            "heap_blks_total",
            "heap_blks_scanned",
            "index_rebuild_count",
        ],
        [],
    ),
    (
        "create_index",
        [
            "pid",
            "datid",
            "datname",
            "relid",
            "index_relid",
            "command",
            "phase",
            "lockers_total",
            "lockers_done",
            "current_locker_pid",
            "blocks_total",
            "blocks_done",
            "tuples_total",
            "tuples_done",
            "partitions_total",
            "partitions_done",
        ],
        [],
    ),
    (
        "basebackup",
        [
            "pid",
            "phase",
            "backup_total",
            "backup_streamed",
            "tablespaces_total",
            "tablespaces_streamed",
        ],
        [],
    ),
    (
        "copy",
        [
            "pid",
            "datid",
            "datname",
            "relid",
            "command",
            "type",
            "bytes_processed",
            "bytes_total",
            "tuples_processed",
            "tuples_excluded",
        ],
        [],
    ),
]


@pytest.mark.backlog_fixed("RP-100")
def test_copy_progress(rust_pg: Any) -> None:
    assert copy_progress(_dsn(rust_pg) + " dbname=postgres") == COPY_PROGRESS


@pytest.mark.backlog_fixed("RP-100")
def test_copy_rows_split_across_messages(rust_pg: Any) -> None:
    assert split_rows(_dsn(rust_pg) + " dbname=postgres") == SPLIT_ROWS


@pytest.mark.backlog_fixed("RP-100")
def test_progress_view_shapes(rust_pg: Any) -> None:
    assert progress_view_shapes(_dsn(rust_pg) + " dbname=postgres") == PROGRESS_VIEW_SHAPES


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print(copy_progress(dsn))
    print(split_rows(dsn))
    print(progress_view_shapes(dsn))
