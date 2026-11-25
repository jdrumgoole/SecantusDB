"""The Rust PG server names an unaliased CASE column as PostgreSQL does
(RP-1046).

`FigureColnameInternal` reads a `CaseExpr`'s ELSE result: a definite name
there (a column, a call, a scalar subquery's column: strength 2) is the
CASE's name, and anything else, no ELSE included, leaves it `case` at
strength 1, which a cast above then replaces with its type's name. Each
answer below is PostgreSQL 15.20's (private cluster, 2026-11-25). The name
reaches a query's row description, a derived table's or CTE's column (and
so what an outer reference or a SQL function body resolves against), and
`pg_get_viewdef`'s `AS`.
"""

from __future__ import annotations

from pathlib import Path

import psycopg
import pytest

from tests.test_rust_pgserver_slice import BINARY, _Server

pytestmark = pytest.mark.skipif(not BINARY.exists(), reason="secantusd-pg not built")

_FROM = " from pg_class c where c.relname = 'pg_class'"

#: Each query and PostgreSQL 15.20's column names.
NAMES: list[tuple[str, list[str]]] = [
    ("select case when c.oid > 5 then 0 else c.relnatts end" + _FROM, ["relnatts"]),
    ("select case when true then 1 else abs(c.relnatts) end" + _FROM, ["abs"]),
    (
        "select case when true then 1 else (case when false then 2 else c.relnatts end) end"
        + _FROM,
        ["relnatts"],
    ),
    ("select case when true then 1 else (select c.relnatts) end" + _FROM, ["relnatts"]),
    ("select case when true then null else xmlconcat('<a/>') end", ["xmlconcat"]),
    ("select (case when true then 1 else c.relnatts end)::int8" + _FROM, ["relnatts"]),
    ("select case when true then 1 else 2 end", ["case"]),
    ("select case when true then 1 end", ["case"]),
    ("select case when true then 1 else 2::int end", ["case"]),
    ("select (case when true then 1 else 2 end)::int8", ["int8"]),
    ("select (1::int4)::int8", ["int8"]),
    (
        "select * from (select case when true then 1 else c.relnatts end" + _FROM + ") s",
        ["relnatts"],
    ),
    (
        "select relnatts from (select case when true then 1 else c.relnatts end" + _FROM + ") s",
        ["relnatts"],
    ),
    (
        "with w as (select case when true then 1 else c.relnatts end" + _FROM + ") select * from w",
        ["relnatts"],
    ),
]


def test_a_case_column_is_named_by_its_else_result(tmp_path: Path) -> None:
    """Each query's column names are PostgreSQL 15.20's. Before RP-1046
    every unaliased CASE was `case`, so `else c.relnatts` hid the column
    from an outer `select relnatts` (42703)."""
    got = []
    with _Server(tmp_path) as server, server.connect() as conn:
        conn.autocommit = True
        for sql, _ in NAMES:
            try:
                cur = conn.execute(sql)
                got.append((sql, [d.name for d in cur.description or []]))
            except psycopg.Error as e:
                got.append((sql, [e.sqlstate or "?"]))
    assert got == NAMES


def test_a_sql_function_body_reads_the_case_column_not_its_parameter(tmp_path: Path) -> None:
    """A derived table's CASE column named `relnatts` is a column, so the
    body reads it rather than the parameter of that name (33 on
    PostgreSQL 15.20, pg_class's own relnatts)."""
    with _Server(tmp_path) as server, server.connect() as conn:
        conn.autocommit = True
        conn.execute(
            "create function f1046(relnatts int) returns int language sql as $$ "
            "select relnatts from (select case when false then 0 else c.relnatts::int end"
            + _FROM
            + ") s $$"
        )
        assert conn.execute("select f1046(-1)").fetchall() == [(33,)]


def test_pg_get_viewdef_names_the_case_column_by_its_else_result(tmp_path: Path) -> None:
    """`AS relnatts`, and a CASE with only a literal ELSE stays `"case"`."""
    with _Server(tmp_path) as server, server.connect() as conn:
        conn.autocommit = True
        conn.execute(
            "create view v1046 as select case when c.oid > 5 then 0 else c.relnatts end,"
            " case when c.oid > 5 then 0 else 1 end from pg_class c"
        )
        got = conn.execute("select pg_get_viewdef('v1046'::regclass)").fetchone()
        assert got == (
            " SELECT\n"
            "        CASE\n"
            "            WHEN (c.oid > (5)::oid) THEN 0\n"
            "            ELSE (c.relnatts)::integer\n"
            "        END AS relnatts,\n"
            "        CASE\n"
            "            WHEN (c.oid > (5)::oid) THEN 0\n"
            "            ELSE 1\n"
            '        END AS "case"\n'
            "   FROM pg_class c;",
        )
