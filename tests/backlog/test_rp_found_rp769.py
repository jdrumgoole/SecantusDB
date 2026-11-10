"""Gap tests of the divergences found while working RP-769 (a domain takes
its base type's collation): RP-782, `CREATE DOMAIN`'s COLLATE clause is
parsed and dropped; RP-783, `information_schema.columns.collation_name`
shows only a declared COLLATE.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster (locale C.UTF-8, built with ICU) with `_SETUP`,
which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t782 cascade",
    "drop table if exists t783 cascade",
    "drop domain if exists d782c cascade",
    "drop domain if exists d782u cascade",
    "drop domain if exists d782i cascade",
    "drop domain if exists d782x cascade",
    "drop domain if exists d783n cascade",
    "create domain d783n as name",
]
_TEARDOWN = 7


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:_TEARDOWN]:
            c.execute(q)


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _answer(c: psycopg.Connection, sql: str) -> object:
    try:
        cur = c.execute(sql)
        return cur.fetchall() if cur.description else cur.statusmessage
    except psycopg.Error as e:
        return f"{e.sqlstate} {e.diag.message_primary}"


# `DefineDomain`: a COLLATE clause names the domain's collation, which its
# array type, its columns, a sort and `pg_collation_for` all take; a base
# type with no collation, or a collation that does not exist, is refused.
@pytest.mark.backlog("RP-782")
def test_create_domain_keeps_its_collate_clause(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                'create domain d782c as text collate "C"',
                'create domain d782u as text collate "und-x-icu"',
                "create table t782 (c d782c, u d782u)",
                "insert into t782 values ('a', 'a'), ('B', 'B')",
                "select typname::text, typcollation::int from pg_type"
                " where typname in ('d782c', '_d782c') order by 1",
                "select attname::text, attcollation::int from pg_attribute"
                " where attrelid = 't782'::regclass and attnum = 1",
                "select u from t782 order by u",
                "select collation for (c), collation for (u) from t782 limit 1",
                "select domain_name::text, collation_name::text from information_schema.domains"
                " where domain_name like 'd782%' order by 1",
                'create domain d782i as int collate "C"',
                'create domain d782x as text collate "nope"',
            ]
        ]
    assert got == [
        "CREATE DOMAIN",
        "CREATE DOMAIN",
        "CREATE TABLE",
        "INSERT 0 2",
        [("_d782c", 950), ("d782c", 950)],
        [("c", 950)],
        [("a",), ("B",)],
        [('"C"', '"und-x-icu"')],
        [("d782c", "C"), ("d782u", "und-x-icu")],
        "42804 collations are not supported by type integer",
        '42704 collation "nope" for encoding "UTF8" does not exist',
    ]


# `information_schema.columns` joins `pg_collation` on `attcollation`, and
# leaves out only `pg_catalog.default`: a `name` column (or array, or
# domain over `name`) shows `C`, and a declared `collate "default"` shows
# nothing.
@pytest.mark.backlog("RP-783")
def test_information_schema_columns_collation_is_the_columns(dsn: str) -> None:
    with _connect(dsn) as c:
        c.execute(
            'create table t783 (n name, t text, tc text collate "C",'
            ' td text collate "default", na name[], dn d783n, i int)'
        )
        got = c.execute(
            "select column_name::text, collation_catalog is not null,"
            " collation_schema::text, collation_name::text"
            " from information_schema.columns where table_name = 't783'"
            " order by ordinal_position"
        ).fetchall()
    assert got == [
        ("n", True, "pg_catalog", "C"),
        ("t", False, None, None),
        ("tc", True, "pg_catalog", "C"),
        ("td", False, None, None),
        ("na", True, "pg_catalog", "C"),
        ("dn", True, "pg_catalog", "C"),
        ("i", False, None, None),
    ]
