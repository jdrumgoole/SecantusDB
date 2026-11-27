"""Gap tests of the divergences found while working RP-1048 (a `text`
column an `INSERT ... SELECT` wrote to a domain-array column was accepted):
RP-1083 (a 42804 assignment error named a domain column by its base type; fixed),
RP-1084 (a value too long for a `varchar(n)` domain was stored in an array
of that domain; fixed) and RP-1085 (a user type created unqualified under a
non-public `search_path` was created in `public`; fixed).

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
    "drop schema if exists s48r cascade",
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


@pytest.mark.backlog_fixed("RP-1083")
def test_domain_column_named_in_42804(dsn: str) -> None:
    """`format_type_be` named the column's own type, the domain; the
    message gave the domain's base type instead."""
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


_QD = 'column "p" is of type q83.qd but expression is of type {}'


@pytest.mark.backlog_fixed("RP-1083")
def test_domain_column_named_in_default_and_qualified(dsn: str) -> None:
    """A DEFAULT's 42804 names the domain too, and a domain outside the
    `search_path` is schema-qualified, as `format_type_be` prints it
    (measured on PostgreSQL 15.20)."""
    setup = [
        *_SETUP,
        "drop table if exists t83d, t83q cascade",
        "drop schema if exists q83 cascade",
        "create schema q83",
        "create domain q83.qd as int",
        "create table t83q (p q83.qd)",
    ]
    try:
        got = {
            q: answer(dsn, q, setup)
            for q in (
                "create table t83d (p pint48 default now())",
                "insert into t83q values ('1'::text)",
                "update t83q set p = true",
                "insert into t83q select '1'::text",
            )
        }
    finally:
        with psycopg.connect(dsn, autocommit=True) as c:
            c.execute("drop table if exists t83d, t83q cascade")
            c.execute("drop schema if exists q83 cascade")
    assert got == {
        "create table t83d (p pint48 default now())": (
            "42804",
            'column "p" is of type pint48 but default expression is of type '
            "timestamp with time zone",
            None,
        ),
        "insert into t83q values ('1'::text)": ("42804", _QD.format("text"), "26"),
        "update t83q set p = true": ("42804", _QD.format("boolean"), "21"),
        "insert into t83q select '1'::text": ("42804", _QD.format("text"), "25"),
    }


_LONG = "value too long for type character varying(3)"


@pytest.mark.backlog_fixed("RP-1084")
def test_domain_array_element_length(dsn: str) -> None:
    """The array coercion applied the element domain's length coercion; the
    overlong element was stored instead."""
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


@pytest.mark.backlog_fixed("RP-1085")
def test_type_created_in_search_path_schema(dsn: str) -> None:
    """The creation namespace of an unqualified name was `public` whatever
    the path; it is now the path's first schema."""
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


# Each statement after `create schema s48r; set search_path = s48r`, as
# PostgreSQL 15.20 answers it: every unqualified type lands in `s48r`, a
# bare name in a DROP / ALTER / COMMENT finds it through the path, and a
# path with no usable schema has nowhere to create.
_ROUND_TRIP = [
    ("create domain d48r as int check (value > 0)", "CREATE DOMAIN"),
    ("create type c48r as (a int)", "CREATE TYPE"),
    ("create type e48r as enum ('a', 'b')", "CREATE TYPE"),
    ("create type r48r as range (subtype = int4)", "CREATE TYPE"),
    ("create type sh48r", "CREATE TYPE"),
    (
        "select string_agg(typname || ':' || typnamespace::regnamespace, ',' order by typname)"
        " from pg_type where typname in ('d48r', 'c48r', 'e48r', 'r48r', 'sh48r')",
        [("c48r:s48r,d48r:s48r,e48r:s48r,r48r:s48r,sh48r:s48r",)],
    ),
    ("create domain d48r as text", ("42710", 'type "d48r" already exists', None)),
    ("alter domain d48r set not null", "ALTER DOMAIN"),
    ("comment on domain d48r is 'x'", "COMMENT"),
    ("alter type e48r add value 'c'", "ALTER TYPE"),
    ("comment on type e48r is 'x'", "COMMENT"),
    ("alter type c48r add attribute b text", "ALTER TYPE"),
    ("alter type e48r rename to e48rb", "ALTER TYPE"),
    ("alter type e48rb owner to postgres", "ALTER TYPE"),
    ("select 'c'::s48r.e48rb::text", [("c",)]),
    ("drop type e48rb", "DROP TYPE"),
    ("drop type c48r", "DROP TYPE"),
    ("drop type r48r", "DROP TYPE"),
    ("drop type sh48r", "DROP TYPE"),
    ("drop domain d48r", "DROP DOMAIN"),
    ("drop type if exists e48rb", "DROP TYPE"),
    ("select count(*) from pg_type where typnamespace = 's48r'::regnamespace", [(0,)]),
    ("set search_path = nosuch48r", "SET"),
    (
        "create type e48rn as enum ('a')",
        ("3F000", "no schema has been selected to create in", None),
    ),
    ("create domain d48rn as int", ("3F000", "no schema has been selected to create in", None)),
    ("set search_path = nosuch48r, s48r", "SET"),
    ("create type e48rn as enum ('a')", "CREATE TYPE"),
    (
        "select typnamespace::regnamespace::text from pg_type where typname = 'e48rn'",
        [("s48r",)],
    ),
    ("drop type e48rn", "DROP TYPE"),
]


@pytest.mark.backlog_fixed("RP-1085")
def test_search_path_type_round_trip(dsn: str) -> None:
    """A type made under a non-public path is found there by every DDL
    statement that names it bare, as `LookupTypeName` finds it."""
    got = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _DROP:
            c.execute(q)
        c.execute("create schema s48r")
        c.execute("set search_path = s48r")
        for q, _ in _ROUND_TRIP:
            try:
                cur = c.execute(q)
                got.append(cur.fetchall() if cur.description else cur.statusmessage)
            except psycopg.Error as e:
                got.append((e.sqlstate, e.diag.message_primary, e.diag.statement_position))
    assert list(zip([q for q, _ in _ROUND_TRIP], got, strict=True)) == _ROUND_TRIP
