"""Batch 55 of the Rust PG server (2026-10-24): the regression tests of
RP-070's cumulative statistics -- the counters behind `pg_stat_*_tables`,
`pg_stat_*_indexes`, `pg_stat_xact_*`, `pg_stat_*_functions`,
`pg_stat_database` and `pg_statio_*` (decided by Joe, 2026-10-22: model
the counters; 2026-10-24: model a buffer cache for the block counts), and
the gap tests of what that model leaves: RP-224 (a LIMIT's scan), RP-225
(the system catalogs' counters) and RP-227 (how a session ended).

A backend's counts reach the shared statistics when it goes idle, at most
once a second, so each scenario forces the flush it reads
(`pg_stat_force_next_flush()`, then the read as the next statement), and
flushes before a VACUUM or ANALYZE, whose report would otherwise land
before or after the pending counts depending on how long each statement
took. The block counters follow PostgreSQL's
mechanism, not its bytes: PostgreSQL also reads visibility-map,
free-space-map and catalog pages, so their scenario asserts what a buffer
cache guarantees -- a read, then hits -- rather than PostgreSQL's numbers.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch55.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made.
"""

from __future__ import annotations

import contextlib
import socket
import sys
import time
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_FLUSH = "select pg_stat_force_next_flush()"
_TABLE = (
    "select seq_scan, seq_tup_read, idx_scan, idx_tup_fetch, n_tup_ins, n_tup_upd, n_tup_del, "
    "n_tup_hot_upd, n_live_tup, n_dead_tup, n_mod_since_analyze, n_ins_since_vacuum, "
    "vacuum_count, analyze_count, last_vacuum is not null, last_analyze is not null "
    "from pg_stat_user_tables where relname = '{}'"
)

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "a table's tuple counters": (
        "RP-070",
        False,
        [
            "create table rp55_t (x int)",
            "insert into rp55_t select generate_series(1, 5)",
            "update rp55_t set x = x + 10 where x = 1",
            "delete from rp55_t where x in (2, 3)",
            "select count(*) from rp55_t",
            _FLUSH,
            _TABLE.format("rp55_t"),
            "drop table rp55_t",
        ],
    ),
    "an aborted transaction's tuples are dead": (
        "RP-070",
        False,
        [
            "create table rp55_a (x int)",
            "insert into rp55_a values (1), (2)",
            "begin",
            "insert into rp55_a values (3), (4), (5)",
            "update rp55_a set x = 0 where x = 1",
            "rollback",
            "begin",
            "insert into rp55_a values (6)",
            "savepoint s",
            "insert into rp55_a values (7)",
            "rollback to s",
            "commit",
            _FLUSH,
            _TABLE.format("rp55_a"),
            "drop table rp55_a",
        ],
    ),
    "a transaction's own counts": (
        "RP-070",
        False,
        [
            "create table rp55_x (x int)",
            _FLUSH,
            "select 1",
            "begin",
            "insert into rp55_x values (1), (2)",
            "delete from rp55_x where x = 1",
            "select seq_scan, seq_tup_read, n_tup_ins, n_tup_upd, n_tup_del, n_tup_hot_upd "
            "from pg_stat_xact_user_tables where relname = 'rp55_x'",
            "commit",
            "drop table rp55_x",
        ],
    ),
    "an index's scans": (
        "RP-070",
        False,
        [
            # The primary key's build scans the (empty) heap: a sequential
            # scan, as CREATE INDEX's is.
            "create table rp55_i (id int primary key, v text)",
            "insert into rp55_i select g, 'v' || g from generate_series(1, 5000) g",
            "create index rp55_iv on rp55_i (v)",
            _FLUSH,
            "select 1",
            "analyze rp55_i",
            "select v from rp55_i where id = 7",
            "select v from rp55_i where id = 8",
            "select count(*) from rp55_i where id = 100000",
            "update rp55_i set v = 'z' where id = 9",
            "delete from rp55_i where id = 10",
            "select count(*) from rp55_i",
            _FLUSH,
            _TABLE.format("rp55_i"),
            "select indexrelname::text, idx_scan, idx_tup_read, idx_tup_fetch "
            "from pg_stat_user_indexes where relname = 'rp55_i' order by 1",
            "drop table rp55_i",
        ],
    ),
    "VACUUM, ANALYZE and TRUNCATE": (
        "RP-070",
        False,
        [
            "create table rp55_v (x int)",
            "insert into rp55_v select generate_series(1, 10)",
            "delete from rp55_v where x <= 4",
            _FLUSH,
            "select 1",
            "vacuum rp55_v",
            _FLUSH,
            _TABLE.format("rp55_v"),
            "insert into rp55_v values (100)",
            _FLUSH,
            "select 1",
            "analyze rp55_v",
            _FLUSH,
            _TABLE.format("rp55_v"),
            "truncate rp55_v",
            _FLUSH,
            _TABLE.format("rp55_v"),
            "drop table rp55_v",
        ],
    ),
    "a reader keeps its first values for the transaction": (
        "RP-070",
        False,
        [
            "create table rp55_s (x int)",
            _FLUSH,
            "select 1",
            "@b begin",
            "@b select n_tup_ins from pg_stat_user_tables where relname = 'rp55_s'",
            "insert into rp55_s values (1)",
            _FLUSH,
            "select 1",
            "@b select n_tup_ins from pg_stat_user_tables where relname = 'rp55_s'",
            "@b select pg_stat_clear_snapshot()",
            "@b select n_tup_ins from pg_stat_user_tables where relname = 'rp55_s'",
            "@b commit",
            "drop table rp55_s",
        ],
    ),
    "a table's counters reset": (
        "RP-070",
        False,
        [
            "create table rp55_r (x int)",
            "insert into rp55_r values (1), (2)",
            _FLUSH,
            "select pg_stat_reset_single_table_counters('rp55_r'::regclass)",
            _TABLE.format("rp55_r"),
            "drop table rp55_r",
        ],
    ),
    "a database's transactions and tuples": (
        "RP-070",
        False,
        [
            # The baseline goes in a table made beforehand: creating one
            # writes catalog rows, which PostgreSQL counts as inserted
            # tuples and this server keeps elsewhere.
            "create table rp55_d (x int)",
            "create table rp55_before (c bigint, r bigint, i bigint, d bigint)",
            _FLUSH,
            "insert into rp55_before select xact_commit, xact_rollback, tup_inserted, "
            "tup_deleted from pg_stat_database where datname = current_database()",
            "insert into rp55_d values (1), (2), (3)",
            "begin",
            "delete from rp55_d",
            "rollback",
            _FLUSH,
            "select d.xact_commit - b.c, d.xact_rollback - b.r, d.tup_inserted - b.i, "
            "d.tup_deleted - b.d from pg_stat_database d, rp55_before b "
            "where d.datname = current_database()",
            # `stats_reset` is left out: it holds the last reset of the
            # database's counters, whatever earlier ran on the server.
            "select numbackends > 0, checksum_failures is null "
            "from pg_stat_database where datname = current_database()",
            "drop table rp55_before",
            "drop table rp55_d",
        ],
    ),
    "a function's calls, tracked when asked": (
        "RP-070",
        False,
        [
            "create function rp55_f(int) returns int language plpgsql "
            "as 'begin return $1 + 1; end'",
            "select rp55_f(1)",
            _FLUSH,
            "select count(*) from pg_stat_user_functions where funcname = 'rp55_f'",
            "set track_functions = 'pl'",
            "select rp55_f(1), rp55_f(2)",
            "select rp55_f(3)",
            _FLUSH,
            "select calls, total_time >= self_time from pg_stat_user_functions "
            "where funcname = 'rp55_f'",
            "reset track_functions",
            "drop function rp55_f(int)",
        ],
    ),
    "a table's pages are read, then hit": (
        "RP-070",
        False,
        [
            "create table rp55_b (x int, pad text)",
            "insert into rp55_b select g, repeat('x', 200) from generate_series(1, 400) g",
            _FLUSH,
            "select heap_blks_read + heap_blks_hit > 0 from pg_statio_user_tables "
            "where relname = 'rp55_b'",
            "create table rp55_hits (h bigint)",
            "insert into rp55_hits select heap_blks_hit from pg_statio_user_tables "
            "where relname = 'rp55_b'",
            "select count(*) from rp55_b",
            _FLUSH,
            "select s.heap_blks_hit - h.h >= 10 from pg_statio_user_tables s, "
            "rp55_hits h where s.relname = 'rp55_b'",
            "drop table rp55_hits",
            "drop table rp55_b",
        ],
    ),
    # ------------------------------------ RP-224 a LIMIT stops a scan early
    "a sequential scan under LIMIT reads only what it returns": (
        "RP-224",
        False,
        [
            "create table rp55_l as select g from generate_series(1, 1000) g",
            _FLUSH,
            "select 1",
            "select * from rp55_l limit 1",
            "select g from rp55_l where g = 10 limit 1",
            _FLUSH,
            "select seq_scan, seq_tup_read from pg_stat_user_tables where relname = 'rp55_l'",
            "drop table rp55_l",
        ],
    ),
    # ------------------------------------ RP-225 the system catalogs' counts
    "a new session's catalog reads are counted": (
        "RP-225",
        True,
        [
            "create table rp55_c (n bigint)",
            _FLUSH,
            "insert into rp55_c select seq_scan + coalesce(idx_scan, 0) from pg_stat_sys_tables "
            "where relname = 'pg_class'",
            "@other select relname from pg_class where relname = 'rp55_c'",
            "@other select pg_stat_force_next_flush()",
            "@other select 1",
            "select s.seq_scan + coalesce(s.idx_scan, 0) > c.n from pg_stat_sys_tables s, rp55_c c "
            "where s.relname = 'pg_class'",
            "drop table rp55_c",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "VACUUM, ANALYZE and TRUNCATE": [
        "CREATE TABLE",
        "INSERT 0 10",
        "DELETE 4",
        [("",)],
        [("1",)],
        "VACUUM",
        [("",)],
        [
            (
                "1",
                "10",
                "None",
                "None",
                "10",
                "0",
                "4",
                "0",
                "6",
                "0",
                "14",
                "0",
                "1",
                "0",
                "True",
                "False",
            )
        ],
        "INSERT 0 1",
        [("",)],
        [("1",)],
        "ANALYZE",
        [("",)],
        [
            (
                "1",
                "10",
                "None",
                "None",
                "11",
                "0",
                "4",
                "0",
                "7",
                "0",
                "0",
                "1",
                "1",
                "1",
                "True",
                "True",
            )
        ],
        "TRUNCATE TABLE",
        [("",)],
        [
            (
                "1",
                "10",
                "None",
                "None",
                "11",
                "0",
                "4",
                "0",
                "0",
                "0",
                "0",
                "0",
                "1",
                "1",
                "True",
                "True",
            )
        ],
        "DROP TABLE",
    ],
    "a database's transactions and tuples": [
        "CREATE TABLE",
        "CREATE TABLE",
        [("",)],
        "INSERT 0 1",
        "INSERT 0 3",
        "BEGIN",
        "DELETE 3",
        "ROLLBACK",
        [("",)],
        [("3", "1", "4", "3")],
        [("True", "True")],
        "DROP TABLE",
        "DROP TABLE",
    ],
    "a function's calls, tracked when asked": [
        "CREATE FUNCTION",
        [("2",)],
        [("",)],
        [("0",)],
        "SET",
        [("2", "3")],
        [("4",)],
        [("",)],
        [("3", "True")],
        "RESET",
        "DROP FUNCTION",
    ],
    "a new session's catalog reads are counted": [
        "CREATE TABLE",
        [("",)],
        "INSERT 0 1",
        [("rp55_c",)],
        [("",)],
        [("1",)],
        [("True",)],
        "DROP TABLE",
    ],
    "a reader keeps its first values for the transaction": [
        "CREATE TABLE",
        [("",)],
        [("1",)],
        "BEGIN",
        [("0",)],
        "INSERT 0 1",
        [("",)],
        [("1",)],
        [("0",)],
        [("",)],
        [("1",)],
        "COMMIT",
        "DROP TABLE",
    ],
    "a sequential scan under LIMIT reads only what it returns": [
        "SELECT 1000",
        [("",)],
        [("1",)],
        [("1",)],
        [("10",)],
        [("",)],
        [("2", "11")],
        "DROP TABLE",
    ],
    "a table's counters reset": [
        "CREATE TABLE",
        "INSERT 0 2",
        [("",)],
        [("",)],
        [
            (
                "0",
                "0",
                "None",
                "None",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "0",
                "False",
                "False",
            )
        ],
        "DROP TABLE",
    ],
    "a table's pages are read, then hit": [
        "CREATE TABLE",
        "INSERT 0 400",
        [("",)],
        [("True",)],
        "CREATE TABLE",
        "INSERT 0 1",
        [("400",)],
        [("",)],
        [("True",)],
        "DROP TABLE",
        "DROP TABLE",
    ],
    "a table's tuple counters": [
        "CREATE TABLE",
        "INSERT 0 5",
        "UPDATE 1",
        "DELETE 2",
        [("3",)],
        [("",)],
        [
            (
                "3",
                "13",
                "None",
                "None",
                "5",
                "1",
                "2",
                "1",
                "3",
                "3",
                "8",
                "5",
                "0",
                "0",
                "False",
                "False",
            )
        ],
        "DROP TABLE",
    ],
    "a transaction's own counts": [
        "CREATE TABLE",
        [("",)],
        [("1",)],
        "BEGIN",
        "INSERT 0 2",
        "DELETE 1",
        [("1", "2", "2", "0", "1", "0")],
        "COMMIT",
        "DROP TABLE",
    ],
    "an aborted transaction's tuples are dead": [
        "CREATE TABLE",
        "INSERT 0 2",
        "BEGIN",
        "INSERT 0 3",
        "UPDATE 1",
        "ROLLBACK",
        "BEGIN",
        "INSERT 0 1",
        "SAVEPOINT",
        "INSERT 0 1",
        "ROLLBACK",
        "COMMIT",
        [("",)],
        [
            (
                "1",
                "5",
                "None",
                "None",
                "7",
                "1",
                "0",
                "1",
                "3",
                "5",
                "3",
                "7",
                "0",
                "0",
                "False",
                "False",
            )
        ],
        "DROP TABLE",
    ],
    "an index's scans": [
        "CREATE TABLE",
        "INSERT 0 5000",
        "CREATE INDEX",
        [("",)],
        [("1",)],
        "ANALYZE",
        [("v7",)],
        [("v8",)],
        [("0",)],
        "UPDATE 1",
        "DELETE 1",
        [("4999",)],
        [("",)],
        [
            (
                "3",
                "9999",
                "5",
                "4",
                "5000",
                "1",
                "1",
                "0",
                "4999",
                "2",
                "2",
                "5000",
                "0",
                "1",
                "False",
                "True",
            )
        ],
        [("rp55_i_pkey", "5", "4", "4"), ("rp55_iv", "0", "0", "0")],
        "DROP TABLE",
    ],
}


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    got = run_scenario(_dsn(rust_pg) + " dbname=postgres", SCENARIOS[name][2])
    assert got == EXPECTED[name]


def sessions_abandoned(dsn: str) -> list[Any]:
    """A session whose client goes away without a Terminate is abandoned
    (`DISCONNECT_CLIENT_EOF`); one ended by `pg_terminate_backend`, killed."""
    with psycopg.connect(dsn, autocommit=True) as conn:
        conn.execute(_FLUSH)
        conn.execute("select 1")
        before = conn.execute(
            "select sessions_abandoned, sessions_killed from pg_stat_database "
            "where datname = current_database()"
        ).fetchone()
        gone = psycopg.connect(dsn, autocommit=True)
        gone.execute("select 1")
        sock = socket.socket(fileno=gone.pgconn.socket)
        sock.shutdown(socket.SHUT_RDWR)
        sock.detach()
        killed = psycopg.connect(dsn, autocommit=True)
        pid = killed.execute("select pg_backend_pid()").fetchone()[0]
        conn.execute("select pg_terminate_backend(%s)", (pid,))
        time.sleep(1.5)
        conn.execute(_FLUSH)
        conn.execute("select 1")
        after = conn.execute(
            "select sessions_abandoned, sessions_killed from pg_stat_database "
            "where datname = current_database()"
        ).fetchone()
        for c in (gone, killed):
            with contextlib.suppress(psycopg.Error):
                c.close()
    return [after[0] - before[0], after[1] - before[1]]


@pytest.mark.backlog_fixed("RP-227")
def test_sessions_abandoned_and_killed(rust_pg: Any) -> None:
    """PostgreSQL 15.19 counts one of each."""
    assert sessions_abandoned(_dsn(rust_pg) + " dbname=postgres") == [1, 1]


@pytest.mark.backlog_fixed("RP-070")
def test_the_counters_outlive_a_clean_restart(tmp_path: Any) -> None:
    """PostgreSQL 15.19 writes its statistics at a clean shutdown and reads
    them back at startup: a table's counters are where they were."""
    from tests.test_rust_pgserver_slice import _Server

    query = (
        "select n_tup_ins, n_tup_del, n_live_tup, n_dead_tup, vacuum_count "
        "from pg_stat_user_tables where relname = 'rp55_p'"
    )
    with (
        _Server(tmp_path) as server,
        psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c,
    ):
        c.execute("create table rp55_p (x int)")
        c.execute("insert into rp55_p select generate_series(1, 5)")
        c.execute("delete from rp55_p where x = 1")
        c.execute(_FLUSH)
        c.execute("select 1")
        c.execute("vacuum rp55_p")
        before = c.execute(query).fetchall()
    with (
        _Server(tmp_path) as server,
        psycopg.connect(_dsn(server) + " dbname=postgres", autocommit=True) as c,
    ):
        after = c.execute(query).fetchall()
    assert before == [(5, 1, 4, 0, 1)]
    assert after == before


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
