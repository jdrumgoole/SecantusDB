"""Gap tests of the divergences found while working RP-846 (a `char(n)`
column's literal DEFAULT is labelled `::bpchar`): a DEFAULT written as an
explicit cast with a modifier loses the modifier, and a `timestamp`,
`timestamptz`, `interval` or `bytea` DEFAULT is printed as the stored value's
internal form instead of a literal.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, the session's TimeZone `UTC`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists y846, w846",
    "set timezone to 'UTC'",
    "create table y846 (e char(4) default 'ab'::char(4), e2 varchar(5) default 'ab'::varchar(5))",
    "create table w846 (ts timestamp(2) default '2020-01-01',"
    " tz timestamptz default '2020-01-01 00:00:00+00',"
    " iv interval default '1 day', by bytea default '\\x01')",
]
_TEARDOWN = 1


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:_TEARDOWN]:
            c.execute(q)


def _defaults(dsn: str, table: str) -> list[object]:
    out: list[object] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        for sql in [
            "select column_name, column_default from information_schema.columns"
            f" where table_name = '{table}' order by ordinal_position",
            "select pg_get_expr(adbin, adrelid) from pg_attrdef"
            f" where adrelid = '{table}'::regclass order by adnum",
        ]:
            try:
                out.append(c.execute(sql).fetchall())
            except psycopg.Error as e:
                out.append(f"{e.sqlstate} {e.diag.message_primary}")
    return out


# An explicit `'ab'::char(4)` is a length coercion marked
# COERCE_EXPLICIT_CAST, which ruleutils prints with its modifier
# (`get_coercion_expr` by `format_type_with_typemod(rettype,
# coercedTypmod)`). Here the modifier is dropped.
@pytest.mark.backlog("RP-863")
def test_an_explicit_cast_default_keeps_its_modifier(dsn: str) -> None:
    assert _defaults(dsn, "y846") == [
        [("e", "'ab'::character(4)"), ("e2", "'ab'::character varying(5)")],
        [("'ab'::character(4)",), ("'ab'::character varying(5)",)],
    ]


# `get_const_expr` prints any constant by its type's output function,
# quoted and labelled. Here a timestamp, timestamptz, interval or bytea
# default prints the stored value's internal form (`DateTime("...")`, an
# interval document, `Binary(0x0, AQ==)`).
@pytest.mark.backlog("RP-864")
def test_a_temporal_or_bytea_default_prints_as_a_literal(dsn: str) -> None:
    assert _defaults(dsn, "w846") == [
        [
            ("ts", "'2020-01-01 00:00:00'::timestamp without time zone"),
            ("tz", "'2020-01-01 00:00:00+00'::timestamp with time zone"),
            ("iv", "'1 day'::interval"),
            ("by", "'\\x01'::bytea"),
        ],
        [
            ("'2020-01-01 00:00:00'::timestamp without time zone",),
            ("'2020-01-01 00:00:00+00'::timestamp with time zone",),
            ("'1 day'::interval",),
            ("'\\x01'::bytea",),
        ],
    ]
