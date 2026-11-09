"""What working RP-737 (a simple CASE and NULLIF over a `char(n)`) fixed,
and what it found:

- a simple CASE's `test = label` and NULLIF's `a = b` compare through the
  operator their two types resolve to, as the binary operators do: a
  `char(n)` against another `char(n)`, an untyped literal or a `varchar`
  is `bpchareq` (trailing blanks ignored on both sides), against `text`
  `texteq` over the `char(n)` side's text (the regression test below);
- NULLIF of a `char(n)` against an untyped literal or NULL was typed
  `text` and dropped its padding, where PostgreSQL keeps the left operand's
  `bpchar` (RP-741, fixed: the untyped side is read as the left operand's
  type, as operator resolution reads it).

An answer is the rows and each column's type OID. Measured on PostgreSQL
15.19 (the shared reference on port 5415): no statement here creates
anything.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

BPCHAR, TEXT, INT2, INT4, REGTYPE = 1042, 25, 21, 23, 2206


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> str:
    return _dsn(rust_pg) + " dbname=postgres"


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        cur = c.execute(query)
        return (cur.fetchall(), [d.type_code for d in cur.description or []])


# Each pair is compared as `=` compares it. The value NULLIF returns is the
# left operand, cast to the `=` operator's left input type: `text` against
# a `text` operand (`nullif('ab'::char(3), 'ab '::text)` is text `ab`).
CASE_AND_NULLIF = {
    "select case 'ab'::char(2) when 'ab'::char(3) then 'eq' else 'ne' end": (
        [("eq",)],
        [TEXT],
    ),
    "select case 'ab'::char(3) when 'ab' then 'eq' else 'ne' end": ([("eq",)], [TEXT]),
    "select case 'ab'::char(3) when 'ab '::varchar then 'eq' else 'ne' end": (
        [("eq",)],
        [TEXT],
    ),
    "select case 'ab '::varchar when 'ab'::char(3) then 'eq' else 'ne' end": (
        [("eq",)],
        [TEXT],
    ),
    "select case 'ab '::text when 'ab'::char(3) then 'eq' else 'ne' end": (
        [("ne",)],
        [TEXT],
    ),
    "select case 'ab'::char(3) when 'ab'::text then 'eq' else 'ne' end": (
        [("eq",)],
        [TEXT],
    ),
    "select case 'ab'::char(3) when 'ab '::text then 'eq' else 'ne' end": (
        [("ne",)],
        [TEXT],
    ),
    "select case 'ab'::char(3) when 'x' then 1 when 'ab ' then 2 else 3 end": (
        [(2,)],
        [INT4],
    ),
    "select case null::char(3) when null then 1 else 2 end": ([(2,)], [INT4]),
    "select k, case c when 'y' then 'eq' else 'ne' end from (values (1, 'y'::char(3))) s(k, c)": (
        [(1, "eq")],
        [INT4, TEXT],
    ),
    "select k from (values (1, 'y'::char(3), 'y '::varchar)) s(k, c, v)"
    " where case c when v then true else false end": ([(1,)], [INT4]),
    "select k from (values (1, 'y'::char(3), 'y'::text)) s(k, c, t)"
    " where case t when c then true else false end": ([(1,)], [INT4]),
    "select nullif('ab'::char(3), 'ab'::varchar)": ([(None,)], [BPCHAR]),
    "select nullif('ab'::char(3), 'ab '::text)": ([("ab",)], [TEXT]),
    "select nullif('ab'::char(3), 'ab'::text)": ([(None,)], [TEXT]),
    "select nullif('ab '::varchar, 'ab'::char(3))": ([(None,)], [BPCHAR]),
    "select nullif('ab'::char(2), 'ab'::char(4))": ([(None,)], [BPCHAR]),
    "select nullif('ab'::char(4), 'zz'::varchar)": ([("ab  ",)], [BPCHAR]),
    "select nullif('ab'::char(4), 'zz'::char(2))": ([("ab  ",)], [BPCHAR]),
    "select k, nullif(c, v) from (values (1, 'y'::char(3), 'y'::varchar)) s(k, c, v)": (
        [(1, None)],
        [INT4, BPCHAR],
    ),
    "select k from (values (1, 'y'::char(3), 'y '::varchar)) s(k, c, v)"
    " where nullif(v, c) is null": ([(1,)], [INT4]),
}


@pytest.mark.backlog_fixed("RP-737")
@pytest.mark.parametrize("query", list(CASE_AND_NULLIF))
def test_case_and_nullif_compare_as_equals_does(query: str, dsn: str) -> None:
    assert answer(dsn, query) == CASE_AND_NULLIF[query]


# `nullif(bpchar, unknown)` resolves `=` to `bpchareq`, the literal read as
# `bpchar`, so the result is the left operand as it is: `character`, its
# padding kept. It was cast to `text` and trimmed. An untyped side reads as
# the left operand's type, not as `unknown`: `nullif(varchar, unknown)` is
# `varchar = varchar`, which resolves to `texteq`, so the result is `text`.
NULLIF_AGAINST_UNTYPED = {
    "select nullif('ab'::char(4), 'zz')": ([("ab  ",)], [BPCHAR]),
    "select pg_typeof(nullif('ab'::char(4), 'zz'))": ([("character",)], [REGTYPE]),
    "select nullif('ab'::char(4), null)": ([("ab  ",)], [BPCHAR]),
    "select nullif('ab'::char(4), 'ab')": ([(None,)], [BPCHAR]),
    "select k, nullif(c, 'zz') from (values (1, 'y'::char(3))) s(k, c)": (
        [(1, "y  ")],
        [INT4, BPCHAR],
    ),
    "select k, nullif(c, null) from (values (1, 'y'::char(3))) s(k, c)": (
        [(1, "y  ")],
        [INT4, BPCHAR],
    ),
    "select octet_length(nullif('ab'::char(4), 'zz'))": ([(4,)], [INT4]),
    "select nullif('a '::varchar, 'b')": ([("a ",)], [TEXT]),
    "select nullif(1::int2, null)": ([(1,)], [INT2]),
}


@pytest.mark.backlog_fixed("RP-741")
@pytest.mark.parametrize("query", list(NULLIF_AGAINST_UNTYPED))
def test_nullif_char_n_against_untyped_keeps_bpchar(query: str, dsn: str) -> None:
    assert answer(dsn, query) == NULLIF_AGAINST_UNTYPED[query]
