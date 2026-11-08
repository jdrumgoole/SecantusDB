"""Gap tests of the divergences found while working RP-570 (a cast to a
domain's array coerces each element through the domain's modifier, which
`test_expression_cast_to_domain_array` pins over a row's expression): a cast
of a COLUMN to such a domain ignores the domain's modifier, and a domain
array literal's input error carries no position. A cast to an array with a
type modifier of its own ignored it until RP-579, and a numeric overflow had
no DETAIL until RP-582; their tests are the regression tests.

An answer is the rows, or `(sqlstate, message, detail, position)` for an
error. The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t570 cascade",
    "drop domain if exists d570c cascade",
    "create domain d570c as char(3)",
    "create table t570 (k int, t text, ta text[])",
    "insert into t570 values (1, 'ab', '{ab}'), (2, 'abcd', '{abcd}')",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:2]:
            c.execute(q)


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_detail,
                e.diag.statement_position,
            )
        # As text: a domain's array is a type psycopg does not parse.
        return [tuple(str(v) for v in row) for row in cur.fetchall()]


_OVERFLOW = (
    "22003",
    "numeric field overflow",
    "A field with precision 5, scale 2 must round to an absolute value less than 10^3.",
    None,
)

# `'{...}'::char(3)[]` is `array_in` at typmod -1 and then the explicit
# length coercion over each element (`coerce_type_typmod` of the array), so
# it truncates and pads as the scalar cast does; `numeric(5,2)` rounds, and
# refuses what does not fit, with apply_typmod's DETAIL and no position.
# Every element came back as written until RP-579.
TYPMOD_ARRAYS = {
    "select '{ab}'::char(3)[]::text": [('{"ab "}',)],
    "select '{abcd}'::char(3)[]::text": [("{abc}",)],
    "select (array['abcd']::char(3)[])::text": [("{abc}",)],
    "select '{abcd}'::varchar(3)[]::text": [("{abc}",)],
    "select '{1.234}'::numeric(5,2)[]::text": [("{1.23}",)],
    "select (array[1.234]::numeric(5,2)[])::text": [("{1.23}",)],
    "select '{123456}'::numeric(5,2)[] is null": _OVERFLOW,
    # The bounds and the nesting are kept, and a NULL element stays NULL.
    "select '[0:1]={abcd,x}'::char(2)[]::text": [('[0:1]={ab,"x "}',)],
    "select '{{abcd,x},{y,NULL}}'::char(2)[]::text": [('{{ab,"x "},{"y ",NULL}}',)],
    # Per row, over an array of the row: each element coerced the same way.
    "select ta::char(3)[]::text from (values (1, '{ab}'::text[]), (2, '{abcd}')) v (k, ta)"
    " order by k": [('{"ab "}',), ("{abc}",)],
}


@pytest.mark.backlog_fixed("RP-579")
@pytest.mark.parametrize("query", list(TYPMOD_ARRAYS))
def test_cast_to_typmod_array(query: str, dsn: str) -> None:
    assert answer(dsn, query) == TYPMOD_ARRAYS[query]


# A cast to a domain's array over an EXPRESSION of a row is evaluated per
# row as a constant's is (RP-570): from an array an explicit element cast,
# which truncates; from text `array_in` through `domain_in` (a CoerceViaIO),
# so a long element is 22001, with no position at run time.
EXPRESSION_CASTS = {
    "select (array[t]::d570c[])::text from t570 order by k": [('{"ab "}',), ("{abc}",)],
    "select ('{' || t || '}')::d570c[] is null from t570": (
        "22001",
        "value too long for type character(3)",
        None,
        None,
    ),
}


@pytest.mark.backlog_fixed("RP-570")
@pytest.mark.parametrize("query", list(EXPRESSION_CASTS))
def test_expression_cast_to_domain_array(query: str, dsn: str) -> None:
    assert answer(dsn, query) == EXPRESSION_CASTS[query]


# A cast of a bare column is the same `coerce_to_domain` as a constant's:
# the base type at the domain's modifier.
COLUMN_CASTS = {
    "select t::d570c, octet_length(t::d570c) from t570 order by k": [
        ("ab ", "3"),
        ("abc", "3"),
    ],
    "select ta::d570c[]::text from t570 order by k": [('{"ab "}',), ("{abc}",)],
}


@pytest.mark.backlog("RP-580")
@pytest.mark.parametrize("query", list(COLUMN_CASTS))
def test_column_cast_to_domain_applies_typmod(query: str, dsn: str) -> None:
    assert answer(dsn, query) == COLUMN_CASTS[query]


# `array_in` reads a domain's element with `domain_in` while it parses the
# literal, so its input error is positioned at the literal, as `'x'::int`
# is; an assignment's 22001 is not.
LITERAL_ERRORS = {
    "select '{abcd}'::d570c[] is null": (
        "22001",
        "value too long for type character(3)",
        None,
        "8",
    ),
}


@pytest.mark.backlog("RP-581")
@pytest.mark.parametrize("query", list(LITERAL_ERRORS))
def test_domain_array_literal_error_position(query: str, dsn: str) -> None:
    assert answer(dsn, query) == LITERAL_ERRORS[query]


# apply_typmod's 22003 explains the overflow in its DETAIL, which the
# server's 22003 lacked until RP-582.
OVERFLOWS = {
    "select '123456'::numeric(5,2)": _OVERFLOW,
    "select 123456::numeric(5,2)": _OVERFLOW,
}


@pytest.mark.backlog_fixed("RP-582")
@pytest.mark.parametrize("query", list(OVERFLOWS))
def test_numeric_overflow_detail(query: str, dsn: str) -> None:
    assert answer(dsn, query) == OVERFLOWS[query]
