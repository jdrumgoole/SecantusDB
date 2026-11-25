"""Gap tests of the divergences found while working RP-1048 (a `text`
column an `INSERT ... SELECT` wrote to a domain-array column was accepted):
RP-1083 (a 42804 assignment error names a domain column by its base type),
RP-1084 (a value too long for a `varchar(n)` domain is stored in an array
of that domain) and RP-1085 (a user type created unqualified under a
non-public `search_path` is created in `public`).

An answer is the command tag for a statement that returns no rows, or
`(sqlstate, message, position)` for an error. The expected answers were
measured on PostgreSQL 15.20 with `_SETUP`, which each statement runs
after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_DROP = [
    "set search_path = public",
    "drop schema if exists s48 cascade",
    "drop table if exists t48, t48v, tt48 cascade",
    "drop type if exists c48, e48 cascade",
    "drop domain if exists pint48, dia48, vd48, d48 cascade",
]

_SETUP = [
    *_DROP,
    "create domain pint48 as int check (value > 0)",
    "create domain dia48 as int[]",
    "create domain vd48 as varchar(3)",
    "create table t48 (p pint48, a dia48)",
    "create table t48v (x vd48[])",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _DROP:
            c.execute(q)


def answer(dsn: str, query: str, setup: list[str] = _SETUP) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in setup:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.statement_position)
        return cur.statusmessage


_P = 'column "p" is of type pint48 but expression is of type text'
_A = 'column "a" is of type dia48 but expression is of type text'


@pytest.mark.backlog("RP-1083")
def test_domain_column_named_in_42804(dsn: str) -> None:
    """`format_type_be` names the column's own type, the domain."""
    got = {
        q: answer(dsn, q)
        for q in (
            "insert into t48(p) values ('1'::text)",
            "update t48 set p = '1'::text",
            "insert into t48(p) select '1'::text",
            "insert into t48(a) values ('{1}'::text)",
        )
    }
    assert got == {
        "insert into t48(p) values ('1'::text)": ("42804", _P, "28"),
        "update t48 set p = '1'::text": ("42804", _P, "20"),
        "insert into t48(p) select '1'::text": ("42804", _P, "27"),
        "insert into t48(a) values ('{1}'::text)": ("42804", _A, "28"),
    }


_LONG = "value too long for type character varying(3)"


@pytest.mark.backlog("RP-1084")
def test_domain_array_element_length(dsn: str) -> None:
    """The array coercion applies the element domain's length coercion."""
    got = {
        q: answer(dsn, q)
        for q in (
            "insert into t48v(x) values (array['abcd'::text])",
            "insert into t48v(x) select array['abcd'::text]",
            "insert into t48v(x) values ('{abcd}')",
        )
    }
    assert got == {
        "insert into t48v(x) values (array['abcd'::text])": ("22001", _LONG, None),
        "insert into t48v(x) select array['abcd'::text]": ("22001", _LONG, None),
        "insert into t48v(x) values ('{abcd}')": ("22001", _LONG, "29"),
    }


@pytest.mark.backlog("RP-1085")
def test_type_created_in_search_path_schema(dsn: str) -> None:
    """The creation namespace of an unqualified name is the path's first schema."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _DROP:
            c.execute(q)
        for q in (
            "create schema s48",
            "set search_path = s48",
            "create domain d48 as int",
            "create type c48 as (a int)",
            "create type e48 as enum ('a')",
        ):
            c.execute(q)
        placed = c.execute(
            "select typname::text, typnamespace::regnamespace::text from pg_type"
            " where typname in ('d48', 'c48', 'e48') order by 1"
        ).fetchall()
    used = answer(
        dsn,
        "create table tt48 (d d48, c c48, e e48)",
        setup=["set search_path = s48"],
    )
    assert (placed, used) == (
        [("c48", "s48"), ("d48", "s48"), ("e48", "s48")],
        "CREATE TABLE",
    )
