"""Gap tests of divergences found while working RP-695 (a PL/pgSQL
assignment to a `varchar(n)[]` variable truncated an overlong element):

- an INSERT or UPDATE of a `varchar(n)[]`, `char(n)[]` or `bit(n)[]`
  column fitted no element to the column's modifier, where PostgreSQL's
  `ArrayCoerceExpr` applies the element's length coercion by the
  assignment rule (fixed by RP-710);
- a PL/pgSQL assignment to an array element, `x[1] := v`, is refused.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t695",
    "create table t695 (k int, a varchar(3)[], c char(3)[], b bit(3)[])",
    "insert into t695 (k) values (1)",
]
_VARCHAR3 = ("error", "22001", "value too long for type character varying(3)")


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str) -> object:
    notes: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.add_notice_handler(lambda d: notes.append(d.message_primary))
        try:
            cur = c.execute(query)
            rows = cur.fetchall() if cur.description else None
            return ("ok", rows, notes)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


# `transformAssignedExpr` coerces the value to the column's type and
# modifier by the assignment rule; for an array that is an `ArrayCoerceExpr`
# over the element's length coercion function.
ARRAY_COLUMN = {
    "insert into t695 (a) values (array['abcdef'])": _VARCHAR3,
    "insert into t695 (a) values ('{abcdef}')": _VARCHAR3,
    "update t695 set a = array['ab', 'abcdef']": _VARCHAR3,
    "insert into t695 (c) values (array['abcd'])": (
        "error",
        "22001",
        "value too long for type character(3)",
    ),
    "insert into t695 (b) values (array[B'1'])": (
        "error",
        "22026",
        "bit string length 1 does not match type bit(3)",
    ),
    "insert into t695 (k, a) values (2, array['abc   ']) returning a": ("ok", [(["abc"],)], []),
}


@pytest.mark.backlog_fixed("RP-710")
@pytest.mark.parametrize("query", list(ARRAY_COLUMN))
def test_array_column_assignment_fits_each_element(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ARRAY_COLUMN[query]


# `exec_assign_value` over a `PLpgSQL_arrayelem`: the new element is
# coerced to the element type and `array_set_element` stores it, extending
# the array (or making one from NULL).
ELEMENT_ASSIGNMENT = {
    "do $$ declare x int[]; begin x[1] := 5; raise notice '%', x; end $$": (
        "ok",
        None,
        ["{5}"],
    ),
    "do $$ declare x text[] := '{a}'; begin x[2] := 'ab'; raise notice '%', x; end $$": (
        "ok",
        None,
        ["{a,ab}"],
    ),
    "do $$ declare x varchar(3)[]; begin x[1] := 'abcdef'; end $$": _VARCHAR3,
}


@pytest.mark.backlog("RP-711")
@pytest.mark.parametrize("query", list(ELEMENT_ASSIGNMENT))
def test_assignment_to_an_array_element(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ELEMENT_ASSIGNMENT[query]
