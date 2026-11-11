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
# that needs it, as `quote_identifier` does. Here it is printed bare.
@pytest.mark.backlog("RP-845")
def test_a_domain_error_quotes_a_name_that_needs_it(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "insert into t839 values ('a', 'b', null)",
            "insert into t839 values ('a', 'b', 0)",
            'select null::"Mx839"',
        ],
    )
    assert got == [
        '23502 domain "Mx839" does not allow null values',
        '23514 value for domain "Mx839" violates check constraint "Mx839_check"',
        '23502 domain "Mx839" does not allow null values',
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
