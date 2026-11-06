"""The regression test of RP-408: a column that does not exist is a 42703
whose HINT names the closest column of the range tables in reach
(`errorMissingColumn` over `searchRangeTableForCol`, a Levenshtein match),
and this server sent no HINT. Found while working RP-405.

An answer is `(sqlstate, message, hint)`. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster, with the
objects `_SETUP` creates, over the simple and the extended protocol.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop view if exists v",
    "drop table if exists e, s, k, w",
    "create table e (a int, b text)",
    "create table s (a int, c text)",
    "create table k (a int, b text, bc int)",
    "create table w (alpha int, beta int, gamma int)",
    "create view v as select a, b as vb from e",
]

_E_B = 'Perhaps you meant to reference the column "e.b".'

SCENARIOS = {
    # A qualified name whose column only the other table has.
    "select s.b from e, s": ("42703", "column s.b does not exist", _E_B),
    "select e.c from e, s": (
        "42703",
        "column e.c does not exist",
        'Perhaps you meant to reference the column "s.c".',
    ),
    # A misspelt name, bare or qualified.
    "select bb from e": ("42703", 'column "bb" does not exist', _E_B),
    "select e.bb from e": ("42703", "column e.bb does not exist", _E_B),
    "select gama from w": (
        "42703",
        'column "gama" does not exist',
        'Perhaps you meant to reference the column "w.gamma".',
    ),
    # Nothing close, or more than half the name different: no hint.
    "select zzzz from e": ("42703", 'column "zzzz" does not exist', None),
    "select alp from w": ("42703", 'column "alp" does not exist', None),
    # Two columns at one distance are both named; three are none.
    "select b from e, s where c = 'x' and bc = 1": (
        "42703",
        'column "bc" does not exist',
        'Perhaps you meant to reference the column "e.b" or the column "s.c".',
    ),
    "select bx from e, k": ("42703", 'column "bx" does not exist', None),
    # A subquery's columns, a star among them.
    "select q.cc from (select a, c from s) q": (
        "42703",
        "column q.cc does not exist",
        'Perhaps you meant to reference the column "q.c".',
    ),
    "select * from (select e.*, s.c from e, s) q where q.cc = 1": (
        "42703",
        "column q.cc does not exist",
        'Perhaps you meant to reference the column "q.c".',
    ),
    # The enclosing query's entries, searched after the subquery's own.
    "select (select bb from s) from e": ("42703", 'column "bb" does not exist', _E_B),
    "select * from e where a in (select aa from s)": (
        "42703",
        'column "aa" does not exist',
        'Perhaps you meant to reference the column "s.a" or the column "e.a".',
    ),
    # An entry the reference cannot see is still searched, once its FROM
    # item has been analysed.
    "select * from e, (select b) x": (
        "42703",
        'column "b" does not exist',
        'There is a column named "b" in table "e",'
        " but it cannot be referenced from this part of the query.",
    ),
    "select * from (select bb) x, e": ("42703", 'column "bb" does not exist', None),
    # A CTE's renamed columns, a view's, a VALUES list's.
    "with t(x, y) as (select a, b from e) select xx from t": (
        "42703",
        'column "xx" does not exist',
        'Perhaps you meant to reference the column "t.x".',
    ),
    "select vbb from v": (
        "42703",
        'column "vbb" does not exist',
        'Perhaps you meant to reference the column "v.vb".',
    ),
    "select * from (values (1, 2)) v where column3 = 1": (
        "42703",
        'column "column3" does not exist',
        'Perhaps you meant to reference the column "v.column1" or the column "v.column2".',
    ),
    # A set operation's earlier arm is an entry, `*SELECT* n`.
    "select 1 as bbb union select bb from e": (
        "42703",
        'column "bb" does not exist',
        'Perhaps you meant to reference the column "e.b" or the column "*SELECT* 1.bbb".',
    ),
    # The writing statements: the target first, then FROM / USING; an
    # INSERT's query is under the target, and DO UPDATE adds `excluded`.
    "update e set a = 1 from s where s.cc = 1": (
        "42703",
        "column s.cc does not exist",
        'Perhaps you meant to reference the column "s.c".',
    ),
    "delete from e returning bb": ("42703", 'column "bb" does not exist', _E_B),
    "insert into e select bb from s": ("42703", 'column "bb" does not exist', _E_B),
    "insert into e values (1, 'x') on conflict (a) do update set b = 'y' returning bb": (
        "42703",
        'column "bb" does not exist',
        'Perhaps you meant to reference the column "e.b" or the column "excluded.b".',
    ),
}


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
    yield dsn
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(_SETUP[0])
        c.execute(_SETUP[1])


def answer(dsn: str, query: str, prepare: bool) -> tuple[str | None, str | None, str | None]:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            c.execute(query, prepare=prepare)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.message_hint)
    return (None, None, None)


@pytest.mark.backlog_fixed("RP-408")
@pytest.mark.parametrize("prepare", [False, True], ids=["simple", "extended"])
@pytest.mark.parametrize("query", list(SCENARIOS))
def test_missing_column_hint(query: str, prepare: bool, dsn: str) -> None:
    assert answer(dsn, query, prepare) == SCENARIOS[query]
