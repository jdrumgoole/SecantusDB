"""Gap tests of the divergences found while working RP-1085 (a user type
created unqualified under a non-public `search_path` was created in
`public`): RP-1218 (a non-public domain's RENAME is a silent no-op),
RP-1219 (a bare type name in a DDL statement reaches `public` when the
path does not), RP-1220 (a type name is not unique across kinds in a
non-public schema) and RP-1222 (a `pg_catalog` written first on the path is
not the creation namespace).

An answer is the rows for a query, the command tag for a statement that
returns none, or `(sqlstate, message, position)` for an error. The
expected answers were measured on PostgreSQL 15.20.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_DROP = [
    "reset search_path",
    "drop schema if exists s85x cascade",
    "drop type if exists pub85x cascade",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _DROP:
            c.execute(q)


def answers(dsn: str, statements: list[str]) -> list[Any]:
    """Each statement's answer, after `_DROP` and `create schema s85x`."""
    out: list[Any] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in [*_DROP, "create schema s85x"]:
            c.execute(q)
        for q in statements:
            try:
                cur = c.execute(q)
                out.append(cur.fetchall() if cur.description else cur.statusmessage)
            except psycopg.Error as e:
                out.append((e.sqlstate, e.diag.message_primary, e.diag.statement_position))
    return out


@pytest.mark.backlog("RP-1218")
def test_rename_non_public_domain(dsn: str) -> None:
    """`ALTER DOMAIN s.d RENAME TO e` renames the domain in its schema."""
    got = answers(
        dsn,
        [
            "create domain s85x.dd as int",
            "alter domain s85x.dd rename to ddb",
            "select typname::text from pg_type"
            " where typnamespace = 's85x'::regnamespace and typtype = 'd'",
        ],
    )
    assert got == ["CREATE DOMAIN", "ALTER DOMAIN", [("ddb",)]]


@pytest.mark.backlog("RP-1219")
def test_bare_drop_type_misses_public_off_path(dsn: str) -> None:
    """With `public` off the path, a bare `DROP TYPE` does not reach it."""
    got = answers(
        dsn,
        [
            "create type public.pub85x as enum ('a')",
            "set search_path = s85x",
            "drop type if exists pub85x",
            "drop type pub85x",
            "reset search_path",
            "select count(*) from pg_type where typname = 'pub85x'",
        ],
    )
    assert got == [
        "CREATE TYPE",
        "SET",
        "DROP TYPE",
        ("42704", 'type "pub85x" does not exist', None),
        "RESET",
        [(1,)],
    ]


@pytest.mark.backlog("RP-1220")
def test_type_name_unique_across_kinds_in_schema(dsn: str) -> None:
    """A schema's types share one namespace: an enum, a composite or a
    domain cannot take a name another kind holds there."""
    got = answers(
        dsn,
        [
            "create domain s85x.dup as int",
            "create type s85x.dup as enum ('x')",
            "create type s85x.dup as (a int)",
            "create type s85x.dup2 as enum ('x')",
            "create domain s85x.dup2 as int",
        ],
    )
    exists = "42710"
    assert got == [
        "CREATE DOMAIN",
        (exists, 'type "dup" already exists', None),
        (exists, 'type "dup" already exists', None),
        "CREATE TYPE",
        (exists, 'type "dup2" already exists', None),
    ]


@pytest.mark.backlog("RP-1222")
def test_explicit_pg_catalog_is_creation_namespace(dsn: str) -> None:
    """A `pg_catalog` written first on the path is where an unqualified
    CREATE lands: a type is made there, a table is refused (42501)."""
    got = answers(
        dsn,
        [
            "set search_path = pg_catalog",
            "create type zz85x as enum ('a')",
            "create table zt85x (a int)",
            "set search_path = pg_catalog, public",
            "create domain zd85x as int",
            "select string_agg(typname || ':' || typnamespace::regnamespace, ',' order by typname)"
            " from pg_type where typname in ('zz85x', 'zd85x')",
            "reset search_path",
            "drop type if exists pg_catalog.zz85x",
            "drop domain if exists pg_catalog.zd85x",
            "drop type if exists public.zz85x",
            "drop domain if exists public.zd85x",
        ],
    )
    assert got[:6] == [
        "SET",
        "CREATE TYPE",
        ("42501", 'permission denied to create "pg_catalog.zt85x"', None),
        "SET",
        "CREATE DOMAIN",
        [("zd85x:pg_catalog,zz85x:pg_catalog",)],
    ]
