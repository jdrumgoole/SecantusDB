"""Gap tests of the divergences found while working RP-782 (`CREATE
DOMAIN`'s COLLATE clause is kept): a cast to a collated domain does not
take its collation; `information_schema`'s `character_data` columns are not
collated "C"; `pg_get_viewdef` of a system view is NULL.

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
    "drop domain if exists xu782f cascade",
    "drop domain if exists xc782f cascade",
    'create domain xu782f as text collate "und-x-icu"',
    'create domain xc782f as text collate "C"',
]
_TEARDOWN = 2


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


# `exprCollation` of a cast to a domain is the domain's `typcollation`, an
# IMPLICIT collation: two different ones meeting in a comparison are 42P22.
# A cast written over a `COLLATE` keeps the clause, EXPLICIT, as
# `coerce_type` pushes the coercion beneath it. Here a cast to a collated
# domain carried no collation, so it compared bytewise; fixed by RP-796.
@pytest.mark.backlog_fixed("RP-796")
def test_a_cast_to_a_collated_domain_takes_its_collation(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "select 'a'::xu782f < 'B'::xu782f, 'a'::xc782f < 'B'::xc782f",
                "select v from (values ('a'::xu782f), ('B'::xu782f)) s(v) order by v",
                "select max(v) from (values ('a'::xu782f), ('B'::xu782f)) s(v)",
                "select 'a'::xu782f < 'B'::text",
                "select 'a'::xu782f < 'B'::xc782f",
                "select ('a' collate \"C\")::xu782f < 'B',"
                " (('a' collate \"und-x-icu\")::xc782f) < 'B'",
                "select ('a' collate \"und-x-icu\")::text < 'B'",
                "select v from (select 'a'::xu782f union all select 'B') s(v) order by v",
                "select a from (values ('a'), ('B'), ('c'), ('D')) t(a)"
                " where a::xu782f < 'C' order by a::xu782f",
            ]
        ]
    assert got == [
        [(True, False)],
        [("a",), ("B",)],
        [("B",)],
        [(True,)],
        "42P22 could not determine which collation to use for string comparison",
        [(False, True)],
        [(True,)],
        [("a",), ("B",)],
        [("a",), ("B",), ("c",)],
    ]


# `information_schema.sql` declares `character_data` and `yes_or_no`
# `COLLATE "C"`, and its views' text columns are of those domains.
@pytest.mark.backlog("RP-797")
def test_information_schema_character_data_columns_are_collated_c(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "select collation for (data_type), collation for (is_nullable),"
                " collation for (table_name) from information_schema.columns limit 1",
                "select collation for (is_insertable_into) from information_schema.tables limit 1",
            ]
        ]
    assert got == [[('"C"', '"C"', '"C"')], [('"C"',)]]


# `pg_get_viewdef` deparses a system view's `_RETURN` rule as it does a
# user's.
@pytest.mark.backlog("RP-798")
def test_pg_get_viewdef_of_a_system_view(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "select pg_get_viewdef('pg_tables'::regclass)",
                "select pg_get_viewdef('information_schema.domains'::regclass) is not null",
            ]
        ]
    assert got == [
        [
            (
                " SELECT n.nspname AS schemaname,\n"
                "    c.relname AS tablename,\n"
                "    pg_get_userbyid(c.relowner) AS tableowner,\n"
                "    t.spcname AS tablespace,\n"
                "    c.relhasindex AS hasindexes,\n"
                "    c.relhasrules AS hasrules,\n"
                "    c.relhastriggers AS hastriggers,\n"
                "    c.relrowsecurity AS rowsecurity\n"
                "   FROM ((pg_class c\n"
                "     LEFT JOIN pg_namespace n ON ((n.oid = c.relnamespace)))\n"
                "     LEFT JOIN pg_tablespace t ON ((t.oid = c.reltablespace)))\n"
                "  WHERE (c.relkind = ANY (ARRAY['r'::\"char\", 'p'::\"char\"]));",
            )
        ],
        [(True,)],
    ]


# `ATExecAddColumn` types an added column of a domain as it does one CREATE
# TABLE declares: an aggregate over the base type takes it.
@pytest.mark.backlog("RP-799")
def test_an_added_domain_column_reaches_its_base_types_aggregates(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "drop table if exists ta782f",
                "create table ta782f (k int)",
                "insert into ta782f values (1), (2)",
                "alter table ta782f add column a xc782f",
                "update ta782f set a = 'x' || k",
                "select string_agg(a, ',' order by a) from ta782f",
                "drop table ta782f",
            ]
        ]
    assert got == [
        "DROP TABLE",
        "CREATE TABLE",
        "INSERT 0 2",
        "ALTER TABLE",
        "UPDATE 2",
        [("x1,x2",)],
        "DROP TABLE",
    ]
