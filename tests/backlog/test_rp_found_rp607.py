"""Gap tests of the divergences found while working RP-607 (a DROP walks a
type's dependents): a domain's array type was missing as a column's type
from `pg_attribute` (fixed, RP-653), and is as a domain's base type from
CREATE DOMAIN.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster; each test makes the objects it names and drops
them after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_RESET = [
    "drop table if exists t607q",
    "drop type if exists ct607q",
    "drop schema if exists s607q cascade",
    "drop domain if exists d607s",
    "drop domain if exists d607q",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _RESET:
            c.execute(q)


def _rows_or_error(c: psycopg.Connection[Any], query: str) -> Any:
    try:
        return c.execute(query).fetchall()
    except psycopg.Error as e:
        return (e.sqlstate, e.diag.message_primary)


@pytest.mark.backlog_fixed("RP-653")
def test_domain_array_column_in_pg_attribute(dsn: str) -> None:
    """A column of a domain's array type was missing from `pg_attribute`:
    the type's oid lookup knew no domain's array, so the row was skipped."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in [*_RESET, "create domain d607q as int"]:
            c.execute(q)
        c.execute("create table t607q (x d607q[], y int)")
        got = _rows_or_error(
            c,
            "select attname, attnum, format_type(atttypid, atttypmod) "
            "from pg_attribute where attrelid = 't607q'::regclass and attnum > 0 "
            "order by attnum",
        )
    assert got == [("x", 1, "d607q[]"), ("y", 2, "integer")]


@pytest.mark.backlog_fixed("RP-653")
def test_domain_array_in_a_schema_and_a_composite(dsn: str) -> None:
    """A schema's domain array was missing from `pg_attribute` as a column,
    and a composite's `ADD ATTRIBUTE` of a domain array was 42704: both
    resolved the type through the same lookup (measured on 15.19)."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in [
            *_RESET,
            "create domain d607q as int",
            "create schema s607q",
            "create domain s607q.dv as text",
            "create type ct607q as (a int)",
        ]:
            c.execute(q)
        c.execute("create table t607q (y int, z s607q.dv[])")
        column = _rows_or_error(
            c,
            "select a.attname, format_type(a.atttypid, a.atttypmod), t.typname "
            "from pg_attribute a join pg_type t on t.oid = a.atttypid "
            "where a.attrelid = 't607q'::regclass and a.attnum > 0 order by a.attnum",
        )
        try:
            c.execute("alter type ct607q add attribute b d607q[]")
            added: Any = "ok"
        except psycopg.Error as e:
            added = (e.sqlstate, e.diag.message_primary)
        attribute = _rows_or_error(
            c,
            "select attname, format_type(atttypid, atttypmod) from pg_attribute "
            "where attrelid = 'ct607q'::regclass order by attnum",
        )
        value = _rows_or_error(c, "select row(1, '{2}')::ct607q::text")
    assert (column, added, attribute, value) == (
        [("y", "integer", "int4"), ("z", "s607q.dv[]", "_dv")],
        "ok",
        [("a", "integer"), ("b", "d607q[]")],
        [("(1,{2})",)],
    )


@pytest.mark.backlog("RP-654")
def test_domain_over_a_domain_array(dsn: str) -> None:
    """A domain's base may be another domain's array type."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in [*_RESET, "create domain d607q as int"]:
            c.execute(q)
        try:
            c.execute("create domain d607s as d607q[]")
            created: Any = "ok"
        except psycopg.Error as e:
            created = (e.sqlstate, e.diag.message_primary)
        base = _rows_or_error(
            c,
            "select format_type(typbasetype, typtypmod) from pg_type where typname = 'd607s'",
        )
        cast = _rows_or_error(c, "select '{1,2}'::d607s::text")
    assert (created, base, cast) == ("ok", [("d607q[]",)], [("{1,2}",)])
