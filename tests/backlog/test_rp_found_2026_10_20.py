"""Gap tests for the Rust PG server items found on 2026-10-20 by the server
audit (`tasks/backlog-rust-postgres.md`, "Found by the 2026-10-20 audit").

Each scenario is a function of a connection (and, where it needs another
database or role, a DSN builder). The expected answers are literals measured
by running the same scenario on a PRIVATE PostgreSQL 15.19 cluster
(`initdb --locale=C -E UTF8`), because most of them create databases, roles
or committed tables, which the shared reference must not be given.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
from collections.abc import Callable
from typing import Any

import psycopg
import pytest

from tests.backlog.conftest import pg_answer

Dsn = Callable[[str, str], str]


def _state(conn: Any, sql: str) -> Any:
    """The SQLSTATE of `sql`, or "ok"."""
    got = pg_answer(conn, sql)
    return "ok" if got[0] == "ok" else got[1]


def _rows(conn: Any, sql: str) -> Any:
    """The rows of `sql`, or its SQLSTATE."""
    got = pg_answer(conn, sql)
    return got[1] if got[0] == "ok" else ("err", got[1])


def _rust_dsn(rust_pg: Any) -> Dsn:
    return lambda user, db: f"host=127.0.0.1 port={rust_pg.port} dbname={db} user={user}"


# --------------------------------------------------- RP-060 infinite times --


def infinite_timestamps(conn: Any, dsn: Dsn) -> tuple:
    conn.execute("create table rp060 (id int, t timestamp, d date)")
    conn.execute(
        "insert into rp060 values (1, 'infinity', 'infinity'), "
        "(2, '2021-01-01', '2021-01-01'), (3, '-infinity', '-infinity')"
    )
    out = (
        _rows(conn, "select id from rp060 where t > '2020-01-01' order by id"),
        _rows(conn, "select (t + interval '1 day')::text from rp060 where id = 1"),
        _rows(conn, "select (d + 1)::text from rp060 where id = 3"),
    )
    conn.execute("create index on rp060 (t)")
    return (
        *out,
        _rows(
            conn,
            "select id from rp060 where t between '-infinity' and 'infinity' order by id",
        ),
    )


# ---------------------------------------------------- RP-061 serializable --


def serializable_write_skew(conn: Any, dsn: Dsn) -> tuple:
    conn.execute("create table rp061 (name text primary key, on_call bool)")
    conn.execute("insert into rp061 values ('a', true), ('b', true)")
    c1 = psycopg.connect(conn.info.dsn, autocommit=True)
    c2 = psycopg.connect(conn.info.dsn, autocommit=True)
    states = []
    try:
        for c in (c1, c2):
            c.execute("begin isolation level serializable")
            c.execute("select count(*) from rp061 where on_call").fetchall()
        for c, who in ((c1, "a"), (c2, "b")):
            states.append(_state(c, f"update rp061 set on_call = false where name = '{who}'"))
        states.append(_state(c1, "commit"))
        states.append(_state(c2, "commit"))
    finally:
        c1.close()
        c2.close()
    # PostgreSQL may report the failure at the second update or at its
    # commit; what matters is that exactly one of the two commits succeeds.
    return ("40001" in states, _rows(conn, "select count(*) from rp061 where on_call"))


# ------------------------------------------------ RP-062 SEARCH and CYCLE --


def search_and_cycle(conn: Any, dsn: Dsn) -> tuple:
    conn.execute("set statement_timeout = '5s'")
    search = _rows(
        conn,
        "with recursive t(n) as (select 1 union all select n + 1 from t where n < 3) "
        "search depth first by n set ord select n, ord::text from t order by n",
    )
    cycle = _rows(
        conn,
        "with recursive g(a, b) as (values (1, 2), (2, 1)), "
        "r(n) as (select 1 union all select g.b from r join g on g.a = r.n) "
        "cycle n set is_cycle using path select n, is_cycle from r order by 1, 2",
    )
    conn.execute("reset statement_timeout")
    return search, cycle


# ------------------------------------------ RP-063 lazy recursive CTEs --


def recursive_with_limit(conn: Any, dsn: Dsn) -> Any:
    conn.execute("set statement_timeout = '5s'")
    got = _rows(
        conn,
        "with recursive t(n) as (select 1 union all select n + 1 from t) select n from t limit 3",
    )
    conn.execute("reset statement_timeout")
    return got


# -------------------------------------------------- RP-064 CREATE DATABASE --


def create_database_options(conn: Any, dsn: Dsn) -> tuple:
    conn.execute("create database rp064_src")
    with psycopg.connect(dsn(conn.info.user, "rp064_src"), autocommit=True) as src:
        src.execute("create table carried (a int)")
        src.execute("insert into carried values (1)")
    conn.execute("create database rp064_dst template rp064_src")
    with psycopg.connect(dsn(conn.info.user, "rp064_dst"), autocommit=True) as dst:
        copied = _rows(dst, "select a from carried")
    conn.execute(
        "create database rp064_l1 template template0 encoding 'LATIN1' "
        "lc_collate 'C' lc_ctype 'C' connection limit 3"
    )
    return (
        copied,
        _rows(
            conn,
            "select pg_encoding_to_char(encoding), datconnlimit from pg_database "
            "where datname = 'rp064_l1'",
        ),
    )


# ------------------------------------------------- RP-065 temp ON COMMIT --


def temp_on_commit(conn: Any, dsn: Dsn) -> tuple:
    conn.execute("begin")
    conn.execute("create temp table rp065_del (a int) on commit delete rows")
    conn.execute("insert into rp065_del values (1)")
    conn.execute("commit")
    kept = _rows(conn, "select count(*) from rp065_del")
    conn.execute("begin")
    conn.execute("create temp table rp065_drop (a int) on commit drop")
    conn.execute("commit")
    return kept, _rows(conn, "select to_regclass('rp065_drop') is null")


# ------------------------------------------------- RP-066 float8 range --


def float8_range(conn: Any, dsn: Dsn) -> tuple:
    conn.execute("create table rp066 (f float8)")
    conn.execute("insert into rp066 values (1e308)")
    return tuple(
        _state(conn, sql)
        for sql in (
            "select '1e308'::float8 * 10",
            "select '1e308'::float8 + '1e308'::float8",
            "select f * 10 from rp066",
            "select '1e-300'::float8 * '1e-300'::float8",
            "select 1e400::numeric::float8",
            "select 1e-400::numeric::float8",
        )
    )


# ------------------------------------- RP-067 default privileges, conn limit --


def default_privileges_and_limits(conn: Any, dsn: Dsn) -> tuple:
    conn.execute("create role rp067_r login")
    conn.execute("alter default privileges grant select on tables to rp067_r")
    conn.execute("create table rp067_t (a int)")
    granted = _rows(conn, "select has_table_privilege('rp067_r', 'rp067_t', 'select')")
    conn.execute("create role rp067_zero login connection limit 0")
    try:
        with psycopg.connect(dsn("rp067_zero", "postgres"), connect_timeout=5):
            login = "ok"
    except psycopg.Error:
        login = "refused"  # 53300; psycopg reports a refused login without a SQLSTATE
    return granted, login


# ----------------------------------------------- RP-068 output settings --


def output_settings(conn: Any, dsn: Dsn) -> tuple:
    out = []
    conn.execute("set intervalstyle = iso_8601")
    out.append(_rows(conn, "select (interval '1 day 2 hours')::text"))
    conn.execute("reset intervalstyle")
    conn.execute("set bytea_output = escape")
    out.append(_rows(conn, "select ('\\x41ff'::bytea)::text"))
    conn.execute("reset bytea_output")
    conn.execute("set extra_float_digits = 0")
    out.append(_rows(conn, "select (0.1::float8 + 0.2::float8)::text"))
    conn.execute("reset extra_float_digits")
    conn.execute("set application_name = 'rp068'")
    out.append(
        _rows(conn, "select application_name from pg_stat_activity where pid = pg_backend_pid()")
    )
    return tuple(out)


# ------------------------------------------------- RP-069 system columns --


def system_columns(conn: Any, dsn: Dsn) -> tuple:
    conn.execute("create table rp069 (a int)")
    conn.execute("insert into rp069 values (1)")
    return tuple(
        _state(conn, f"select {col} from rp069")
        for col in ("ctid", "xmin", "xmax", "cmin", "cmax", "tableoid")
    )


# ----------------------------------------------- RP-070 catalog relations --

CATALOG_RELATIONS = (
    pathlib.Path(__file__).with_name("rp070_catalog_relations.txt").read_text().split()
)


def catalog_relations(conn: Any, dsn: Dsn) -> list:
    return [
        name for name in CATALOG_RELATIONS if _state(conn, f"select * from {name} limit 0") != "ok"
    ]


# --------------------------------------------------- RP-071 client tools --


def _pg15_bin() -> pathlib.Path:
    for candidate in (os.environ.get("SECANTUS_PG15_BIN"), "/opt/homebrew/opt/postgresql@15/bin"):
        if candidate and (pathlib.Path(candidate) / "pg_dump").exists():
            return pathlib.Path(candidate)
    raise RuntimeError("PostgreSQL 15 client tools not found (set SECANTUS_PG15_BIN)")


TOOLS = {
    "pg_dump --schema-only": ["pg_dump", "--schema-only"],
    "pg_dumpall --globals-only": ["pg_dumpall", "--globals-only"],
    "pgbench -i": ["pgbench", "-i", "-s", "1"],
    "psql \\l": ["psql", "-c", "\\l"],
    "psql \\l+": ["psql", "-c", "\\l+"],
    "psql \\dd": ["psql", "-c", "\\dd"],
    "psql \\dc": ["psql", "-c", "\\dc"],
    "psql \\dL": ["psql", "-c", "\\dL"],
    "psql \\dy": ["psql", "-c", "\\dy"],
    "psql \\db": ["psql", "-c", "\\db"],
}


def client_tools(port: int, user: str) -> list:
    bindir = _pg15_bin()
    failed = []
    for name, argv in TOOLS.items():
        exe = shutil.which(argv[0], path=str(bindir))
        proc = subprocess.run(
            [exe, *argv[1:], "-h", "127.0.0.1", "-p", str(port), "-U", user]
            + ([] if argv[0] == "pg_dumpall" else ["postgres"]),
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        if proc.returncode != 0:
            failed.append(name)
    return failed


# ---------------------------------------------- RP-072 refused statements --


def refused_statements(conn: Any, dsn: Dsn) -> list:
    setup = (
        "create role rp072_r",
        "create table rp072_t (a int primary key, b text)",
        "insert into rp072_t values (1, 'x')",
        "create index rp072_i on rp072_t (b)",
        "create type rp072_ty as (a int)",
    )
    for sql in setup:
        conn.execute(sql)
    statements = (
        "alter system set work_mem = '8MB'",
        "alter system reset work_mem",
        "alter role rp072_r set work_mem = '8MB'",
        "reassign owned by rp072_r to current_user",
        "drop owned by rp072_r",
        "alter type rp072_ty add attribute b text",
        "alter type rp072_ty rename attribute a to c",
        "create text search configuration rp072_ts (copy = english)",
        "alter table rp072_t set unlogged",
        "alter table rp072_t set logged",
        "alter table rp072_t set (fillfactor = 70)",
        "alter table rp072_t alter column b set storage external",
        "alter table rp072_t alter column b set statistics 100",
        "alter table rp072_t cluster on rp072_i",
    )
    refused = [sql for sql in statements if _state(conn, sql) == "0A000"]
    conn.execute("begin")
    conn.execute("declare rp072_c cursor for select * from rp072_t for update")
    conn.execute("fetch next from rp072_c")
    if _state(conn, "update rp072_t set b = 'y' where current of rp072_c") == "0A000":
        refused.append("where current of")
    else:
        conn.execute("rollback")
    if conn.info.transaction_status != psycopg.pq.TransactionStatus.IDLE:
        conn.execute("rollback")
    return refused


# --------------------------------------------------- RP-073 COPY options --


def copy_options(conn: Any, dsn: Dsn) -> list:
    conn.execute("create table rp073 (a int, b text)")
    conn.execute("insert into rp073 values (1, 'x'), (2, null)")
    refused = []
    cur = conn.cursor()

    def run(name: str, fn: Callable[[], Any]) -> None:
        try:
            fn()
        except psycopg.Error as e:
            if e.sqlstate == "0A000":
                refused.append(name)
            if conn.info.transaction_status != psycopg.pq.TransactionStatus.IDLE:
                conn.execute("rollback")

    def copy_out(sql: str) -> None:
        with cur.copy(sql) as cp:
            for _ in cp:
                pass

    def copy_in(sql: str, data: str) -> None:
        with cur.copy(sql) as cp:
            cp.write(data)

    run("FORCE_QUOTE", lambda: copy_out("copy rp073 to stdout (format csv, force_quote *)"))
    run(
        "FORCE_NOT_NULL",
        lambda: copy_in("copy rp073 from stdin (format csv, force_not_null (b))", "3,\n"),
    )
    run(
        "FORCE_NULL",
        lambda: copy_in("copy rp073 from stdin (format csv, force_null (b))", '4,""\n'),
    )

    def freeze() -> None:
        conn.execute("begin")
        conn.execute("truncate rp073")
        copy_in("copy rp073 from stdin (freeze)", "5\ty\n")
        conn.execute("commit")

    run("FREEZE", freeze)
    return refused


# ------------------------------------------------- RP-074 smaller answers --


def smaller_answers(conn: Any, dsn: Dsn) -> tuple:
    def name(sql: str) -> Any:
        try:
            return conn.execute(sql).description[0].name
        except psycopg.Error as e:
            return ("err", e.sqlstate)

    conn.execute("create table rp074 (a int)")
    conn.execute("create materialized view rp074_mv as select a from rp074")
    return (
        _rows(conn, "select make_interval(days => 1)::text"),
        name("select '1.2.3.0/24'::inet >> '1.2.3.4'::inet"),
        name("select case when true then 1 end"),
        name("select xmlagg(x) from (values ('<a/>'::xml)) v(x)"),
        _rows(
            conn,
            "select pg_column_size('abc'::text), pg_column_size(row(1, 2)), "
            "pg_column_size(array[1, 2, 3]), pg_column_size(1.5::numeric)",
        ),
        _state(conn, "refresh materialized view concurrently rp074_mv"),
    )


# ===================================================================== tests


#: Each scenario's answer on PostgreSQL 15.19 (a private cluster, `initdb
#: --locale=C -E UTF8`), measured 2026-10-20.
EXPECTED: dict[str, Any] = {
    "infinite_timestamps": (
        [(1,), (2,)],
        [("infinity",)],
        [("-infinity",)],
        [(1,), (2,), (3,)],
    ),
    "serializable_write_skew": (True, [(1,)]),
    "search_and_cycle": (
        [(1, "{(1)}"), (2, "{(1),(2)}"), (3, "{(1),(2),(3)}")],
        [(1, False), (1, True), (2, False)],
    ),
    "recursive_with_limit": [(1,), (2,), (3,)],
    "create_database_options": ([(1,)], [("LATIN1", 3)]),
    "temp_on_commit": ([(0,)], [(True,)]),
    "float8_range": ("22003",) * 6,
    "default_privileges_and_limits": ([(True,)], "refused"),
    "output_settings": ([("P1DT2H",)], [("A\\377",)], [("0.3",)], [("rp068",)]),
    "system_columns": ("ok",) * 6,
    "catalog_relations": [],
    "refused_statements": [],
    "copy_options": [],
    "smaller_answers": (
        [("1 day",)],
        "?column?",
        "case",
        "xmlagg",
        [(7, 32, 36, 10)],
        "55000",
    ),
}


def _check(name: str, rust_pg: Any) -> None:
    with rust_pg.connect() as conn:
        got = globals()[name](conn, _rust_dsn(rust_pg))
    assert got == EXPECTED[name]


@pytest.mark.backlog("RP-060")
def test_infinite_timestamps(rust_pg: Any) -> None:
    """PostgreSQL 15.19 orders `infinity` / `-infinity` with finite values,
    adds to them, and an index on the column returns all three rows."""
    _check("infinite_timestamps", rust_pg)


@pytest.mark.backlog("RP-061")
def test_serializable_write_skew(rust_pg: Any) -> None:
    """PostgreSQL 15.19 fails one of two SERIALIZABLE transactions that each
    read both rows and write one (40001), so one row stays on call."""
    _check("serializable_write_skew", rust_pg)


@pytest.mark.backlog_fixed("RP-062")
def test_recursive_search_and_cycle(rust_pg: Any) -> None:
    """PostgreSQL 15.19 adds the SEARCH column and stops a CYCLE."""
    _check("search_and_cycle", rust_pg)


@pytest.mark.backlog_fixed("RP-063")
def test_recursive_cte_under_limit(rust_pg: Any) -> None:
    """PostgreSQL 15.19 answers an unbounded recursion under LIMIT 3 at once."""
    _check("recursive_with_limit", rust_pg)


@pytest.mark.backlog("RP-064")
def test_create_database_options(rust_pg: Any) -> None:
    """PostgreSQL 15.19 copies a TEMPLATE's tables and records ENCODING and
    CONNECTION LIMIT."""
    _check("create_database_options", rust_pg)


@pytest.mark.backlog_fixed("RP-065")
def test_temp_table_on_commit(rust_pg: Any) -> None:
    """PostgreSQL 15.19 empties an ON COMMIT DELETE ROWS table and drops an
    ON COMMIT DROP one at commit."""
    _check("temp_on_commit", rust_pg)


@pytest.mark.backlog_fixed("RP-066")
def test_float8_out_of_range(rust_pg: Any) -> None:
    """PostgreSQL 15.19 raises 22003 for float8 overflow and underflow, in
    arithmetic and in a numeric -> float8 cast."""
    _check("float8_range", rust_pg)


@pytest.mark.backlog("RP-067")
def test_default_privileges_and_connection_limit(rust_pg: Any) -> None:
    """PostgreSQL 15.19 applies ALTER DEFAULT PRIVILEGES to a table created
    afterwards, and refuses a login past a role's CONNECTION LIMIT."""
    _check("default_privileges_and_limits", rust_pg)


@pytest.mark.backlog_fixed("RP-068")
def test_output_settings(rust_pg: Any) -> None:
    """PostgreSQL 15.19 honours IntervalStyle, bytea_output and
    extra_float_digits, and shows application_name in pg_stat_activity."""
    _check("output_settings", rust_pg)


@pytest.mark.backlog("RP-069")
def test_system_columns(rust_pg: Any) -> None:
    """PostgreSQL 15.19 answers ctid, xmin, xmax, cmin, cmax and tableoid."""
    _check("system_columns", rust_pg)


@pytest.mark.backlog("RP-070")
def test_catalog_relations_are_queryable(rust_pg: Any) -> None:
    """Every pg_catalog / information_schema table and view in PostgreSQL
    15.19's pg_class (rp070_catalog_relations.txt) can be selected from."""
    _check("catalog_relations", rust_pg)


@pytest.mark.backlog("RP-071")
def test_client_tools(rust_pg: Any) -> None:
    """PostgreSQL 15's pg_dump, pg_dumpall -g, pgbench -i and these psql
    meta-commands all exit 0 against PostgreSQL 15.19."""
    assert client_tools(rust_pg.port, "test") == []


@pytest.mark.backlog("RP-072")
def test_statements_not_refused(rust_pg: Any) -> None:
    """PostgreSQL 15.19 runs each of these statements; none is 0A000."""
    _check("refused_statements", rust_pg)


@pytest.mark.backlog_fixed("RP-073")
def test_copy_options(rust_pg: Any) -> None:
    """PostgreSQL 15.19 takes FORCE_QUOTE, FORCE_NOT_NULL, FORCE_NULL and
    FREEZE."""
    _check("copy_options", rust_pg)


@pytest.mark.backlog_fixed("RP-074")
def test_smaller_answers(rust_pg: Any) -> None:
    """PostgreSQL 15.19: named arguments to a built-in, its result column
    names, pg_column_size, and REFRESH ... CONCURRENTLY without a unique
    index (55000)."""
    _check("smaller_answers", rust_pg)
