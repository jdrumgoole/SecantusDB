"""Gap tests of the divergences found while working RP-839 (a column `LIKE`
copies from a column of a domain is one of the domain): a `LIKE` copy drops
a column's collation, a domain whose name needs quotes is named unquoted in
its NOT NULL and CHECK errors, and a `char(n)` column's DEFAULT renders its
cast as `::character`.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each test runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists l839, t839, z839",
    'drop domain if exists "Mx839" cascade',
    'create domain "Mx839" as int not null check (value > 0)',
    'create table t839 (x text collate "C", y text, m "Mx839")',
    "create table z839 (o char(4) default 'ab', v varchar(3) default 'q')",
]
_TEARDOWN = 2


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:_TEARDOWN]:
            c.execute(q)


def _answers(dsn: str, sqls: list[str]) -> list[object]:
    out: list[object] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        for sql in sqls:
            try:
                cur = c.execute(sql)
                out.append(cur.fetchall() if cur.description else cur.statusmessage)
            except psycopg.Error as e:
                out.append(f"{e.sqlstate} {e.diag.message_primary}")
    return out


# `transformTableLikeClause` copies each column's `attcollation` with its
# type, so a `text COLLATE "C"` column's copy is collated "C", which
# `information_schema.columns.collation_name` names. The copy took its
# type's default collation until RP-844.
@pytest.mark.backlog_fixed("RP-844")
def test_a_like_copy_keeps_a_columns_collation(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "create table l839 (like t839)",
            "select attname, attcollation::regcollation::text from pg_attribute"
            " where attrelid = 'l839'::regclass and attnum > 0 order by attnum",
            "select collation_name from information_schema.columns"
            " where table_name = 'l839' and column_name = 'x'",
        ],
    )
    assert got == [
        "CREATE TABLE",
        [("x", '"C"'), ("y", '"default"'), ("m", "-")],
        [("C",)],
    ]


# The domain errors name the domain by `format_type_be`, which quotes a name
# that needs it, as `quote_identifier` does: a mixed-case name and a
# keyword (`"int"` in a schema off the path, `"select"`). It was printed
# bare until RP-845.
@pytest.mark.backlog_fixed("RP-845")
def test_a_domain_error_quotes_a_name_that_needs_it(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "insert into t839 values ('a', 'b', null)",
            "insert into t839 values ('a', 'b', 0)",
            'select null::"Mx839"',
            "drop schema if exists s845 cascade",
            'drop domain if exists "select"',
            "create schema s845",
            'create domain s845."int" as int check (value > 0)',
            'create domain "select" as int not null',
            'select 0::s845."int"',
            'select null::"select"',
            "drop schema s845 cascade",
            'drop domain "select"',
        ],
    )
    assert got == [
        '23502 domain "Mx839" does not allow null values',
        '23514 value for domain "Mx839" violates check constraint "Mx839_check"',
        '23502 domain "Mx839" does not allow null values',
        "DROP SCHEMA",
        "DROP DOMAIN",
        "CREATE SCHEMA",
        "CREATE DOMAIN",
        "CREATE DOMAIN",
        '23514 value for domain s845."int" violates check constraint "int_check"',
        '23502 domain "select" does not allow null values',
        "DROP SCHEMA",
        "DROP DOMAIN",
    ]


# One `CoerceToDomain` checks a domain over a domain, the inner domain's
# constraints included, and names the domain it coerces to -- its
# `resulttype` by `format_type_be`, quoted -- however the coercion is
# reached: a column's INSERT, a cast or an array's element cast (a PL/pgSQL
# variable's is in `test_rp_found_rp664.py` / `test_rp_found_rp694.py`).
# Measured on PostgreSQL 15.19 (RP-845).
@pytest.mark.backlog_fixed("RP-845")
def test_a_domain_over_a_domain_names_the_outer_one_quoted(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "drop table if exists o845",
            'drop domain if exists "Cc845", c845',
            'create domain "Over845" as "Mx839"',
            "create domain c845 as char(3) check (value <> 'abc')",
            'create domain "Cc845" as c845',
            'create table o845 (m "Over845")',
            "insert into o845 values (0)",
            'select 0::"Over845"',
            'select null::"Over845"',
            "select 'abcd'::\"Cc845\"",
            'select array[0]::"Over845"[]',
            "select array['abcd']::\"Cc845\"[]",
            "drop table o845",
            'drop domain "Cc845", c845',
        ],
    )
    check = 'value for domain "Over845" violates check constraint "Mx839_check"'
    char_check = 'value for domain "Cc845" violates check constraint "c845_check"'
    assert got == [
        "DROP TABLE",
        "DROP DOMAIN",
        "CREATE DOMAIN",
        "CREATE DOMAIN",
        "CREATE DOMAIN",
        "CREATE TABLE",
        f"23514 {check}",
        f"23514 {check}",
        '23502 domain "Over845" does not allow null values',
        f"23514 {char_check}",
        f"23514 {check}",
        f"23514 {char_check}",
        "DROP TABLE",
        "DROP DOMAIN",
    ]


# `ruleutils` prints a constant's cast by `format_type_with_typemod` of the
# Const's type with its modifier, -1, so `bpchar` for a `char(n)` column's
# DEFAULT (the column's modifier sits on the implicit length coercion,
# which ruleutils does not print). It printed the SQL spelling `character`
# until RP-846.
@pytest.mark.backlog_fixed("RP-846")
def test_a_char_columns_default_casts_to_bpchar(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "select column_name, column_default from information_schema.columns"
            " where table_name = 'z839' order by ordinal_position",
            "select pg_get_expr(adbin, adrelid) from pg_attrdef"
            " where adrelid = 'z839'::regclass order by adnum",
        ],
    )
    assert got == [
        [("o", "'ab'::bpchar"), ("v", "'q'::character varying")],
        [("'ab'::bpchar",), ("'q'::character varying",)],
    ]


# PL/pgSQL reads a declaration's type with `parse_datatype`, which keeps a
# quoted identifier's case, and a domain's errors name it quoted. Here the
# quoted name is folded to lower case and not found (42704 `type "over845"
# does not exist`).
@pytest.mark.backlog("RP-865")
def test_a_plpgsql_declaration_keeps_a_quoted_types_case(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            'create domain "Over845" as "Mx839"',
            "do $$ declare x \"Over845\" := 5; begin raise notice '%', x; end $$",
            'do $$ declare x "Over845" := 1; begin x := 0; end $$',
            'do $$ declare x "Over845"; begin end $$',
        ],
    )
    assert got == [
        "CREATE DOMAIN",
        "DO",
        '23514 value for domain "Over845" violates check constraint "Mx839_check"',
        '23502 domain "Over845" does not allow null values',
    ]
