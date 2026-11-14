"""Gap tests of the divergences found while working RP-769 (a domain takes
its base type's collation): RP-782, `CREATE DOMAIN`'s COLLATE clause was
parsed and dropped (fixed); RP-783, `information_schema.columns.collation_name`
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
# The clause was parsed and dropped, every domain taking its base type's
# collation (RP-782).
@pytest.mark.backlog_fixed("RP-782")
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


# `GetColumnDefCollation`: a column's collation is its own COLLATE's, else
# its type's -- through a domain over a domain, and for a column ALTER TABLE
# ADD COLUMN adds, whose own COLLATE was dropped too (RP-782).
@pytest.mark.backlog_fixed("RP-782")
def test_a_column_takes_its_domains_collate_clause(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "drop table if exists te782",
                "drop domain if exists e782c cascade",
                "drop domain if exists e782u cascade",
                'create domain e782c as text collate "C"',
                "create domain e782d as e782c",
                'create domain e782u as e782c collate "und-x-icu"',
                'create table te782 (c e782c, d e782d, u e782u, o e782c collate "und-x-icu")',
                "insert into te782 values ('a', 'a', 'a', 'a'), ('B', 'B', 'B', 'B')",
                "select attname::text, attcollation::int from pg_attribute"
                " where attrelid = 'te782'::regclass and attnum > 0 order by attnum",
                "select collation for (c), collation for (d), collation for (u),"
                " collation for (o) from te782 limit 1",
                "select string_agg(c, ',' order by c), string_agg(d, ',' order by d),"
                " string_agg(u, ',' order by u), string_agg(o, ',' order by o) from te782",
                "alter table te782 add column a e782u",
                'alter table te782 add column z text collate "C"',
                'alter table te782 add column zu e782c collate "und-x-icu"',
                "update te782 set a = c, z = c, zu = c",
                "select attname::text, attcollation::int from pg_attribute"
                " where attrelid = 'te782'::regclass and attnum > 4 order by attnum",
                # (an added domain column is read by a sort, not string_agg:
                # RP-799)
                "select a from te782 order by a",
                "select z from te782 order by z",
                "select zu from te782 order by zu",
                'alter table te782 add column w int collate "C"',
                'alter table te782 add column w text collate "nope"',
                "drop table te782",
                "drop domain e782c cascade",
            ]
        ]
    assert got == [
        "DROP TABLE",
        "DROP DOMAIN",
        "DROP DOMAIN",
        "CREATE DOMAIN",
        "CREATE DOMAIN",
        "CREATE DOMAIN",
        "CREATE TABLE",
        "INSERT 0 2",
        [("c", 950), ("d", 950), ("u", 12713), ("o", 12713)],
        [('"C"', '"C"', '"und-x-icu"', '"und-x-icu"')],
        [("B,a", "B,a", "a,B", "a,B")],
        "ALTER TABLE",
        "ALTER TABLE",
        "ALTER TABLE",
        "UPDATE 2",
        [("a", 12713), ("z", 950), ("zu", 12713)],
        [("a",), ("B",)],
        [("B",), ("a",)],
        [("a",), ("B",)],
        "42804 collations are not supported by type integer",
        '42704 collation "nope" for encoding "UTF8" does not exist',
        "DROP TABLE",
        "DROP DOMAIN",
    ]


# `information_schema.columns` joins `pg_collation` on `attcollation`, and
# leaves out only `pg_catalog.default`: a `name` column (or array, or
# domain over `name`) shows `C`, and a declared `collate "default"` shows
# nothing. Here only a declared COLLATE showed; fixed by RP-783.
@pytest.mark.backlog_fixed("RP-783")
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
