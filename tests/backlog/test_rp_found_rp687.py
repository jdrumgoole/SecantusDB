"""Gap tests of divergences found while working RP-687 (a PL/pgSQL
assignment to a `varchar(n)` / `char(n)` variable truncated an overlong
value):

- `SELECT ... INTO` a scalar variable stored the column's value as it was,
  so an overlong one was kept truncated and a domain's CHECK was not
  checked, where PostgreSQL assigns it by the assignment rule (RP-694,
  fixed);
- an assignment to a variable of `varchar(n)[]` truncates each overlong
  element where PostgreSQL raises 22001 (RP-695);
- a cast to a domain over a domain names the INNER domain in its 23514,
  where PostgreSQL names the one cast to (RP-696).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop domain if exists dcc687, dvv687 cascade",
    "create domain dcc687 as varchar(3) check (value <> 'no')",
    "create domain dvv687 as dcc687",
]
_VARCHAR3 = ("error", "22001", "value too long for type character varying(3)")


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop table if exists t694")
        c.execute("drop domain if exists dpos694 cascade")
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            return c.execute(query).statusmessage
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


# `exec_move_row` assigns each column to its variable with
# `exec_assign_value`, the assignment rule `:=` uses.
SELECT_INTO = {
    "do $$ declare x varchar(3); begin select 'abcdef' into x; end $$": _VARCHAR3,
    "do $$ declare x dcc687; begin select 'no' into x; end $$": (
        "error",
        "23514",
        'value for domain dcc687 violates check constraint "dcc687_check"',
    ),
}


@pytest.mark.backlog_fixed("RP-694")
@pytest.mark.parametrize("query", list(SELECT_INTO))
def test_select_into_variable_assigns_by_the_assignment_rule(query: str, dsn: str) -> None:
    """Was: the first stored `abc` and the second `no` (RP-694)."""
    assert _answer(dsn, query) == SELECT_INTO[query]


# Every way a query's row moves into variables goes through
# `exec_move_row` -> `exec_assign_value`: a SELECT, a FETCH, a RETURNING,
# a list of variables, a column of a table. Measured on a fresh 15.19
# cluster with `_MOVE_SETUP`; the second element is the notices raised.
_MOVE_SETUP = [
    "drop table if exists t694",
    "drop domain if exists dpos694 cascade",
    "create domain dpos694 as int check (value > 0)",
    "create table t694 (k int, d dcc687, p dpos694, v varchar(10), ch char(4), n numeric(6,2))",
    "insert into t694 values (1, 'ok', 5, 'abcdef', 'ab', 12.345)",
]


def _moved(dsn: str, query: str) -> object:
    notes: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP + _MOVE_SETUP:
            c.execute(q)
        c.add_notice_handler(lambda d: notes.append(d.message_primary))
        try:
            c.execute(query)
            return ("ok", notes)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


MOVED = {
    "do $$ declare x numeric(3,1); begin select 12345 into x; end $$": (
        "error",
        "22003",
        "numeric field overflow",
    ),
    "do $$ declare x int; begin select 'abc'::text into x; end $$": (
        "error",
        "22P02",
        'invalid input syntax for type integer: "abc"',
    ),
    "do $$ declare x int; begin select 5000000000::bigint into x; end $$": (
        "error",
        "22003",
        "integer out of range",
    ),
    "do $$ declare x int; y varchar(2); begin select 1, 'abcdef' into x, y; end $$": (
        "error",
        "22001",
        "value too long for type character varying(2)",
    ),
    "do $$ declare x bit(3); begin select B'10101' into x; end $$": (
        "error",
        "22026",
        "bit string length 5 does not match type bit(3)",
    ),
    "do $$ declare x char(2); begin select 'abc ' into x; end $$": (
        "error",
        "22001",
        "value too long for type character(2)",
    ),
    "do $$ declare x varchar(3); begin select 'abcdef' into strict x; end $$": _VARCHAR3,
    "do $$ declare x varchar(3); begin select v into x from t694; end $$": _VARCHAR3,
    "do $$ declare c cursor for select v from t694; x varchar(3);"
    " begin open c; fetch c into x; end $$": _VARCHAR3,
    "do $$ declare x varchar(3); begin update t694 set k = k returning v into x; end $$": (
        _VARCHAR3
    ),
    "do $$ declare x dpos694; begin select -p into x from t694; end $$": (
        "error",
        "23514",
        'value for domain dpos694 violates check constraint "dpos694_check"',
    ),
    "do $$ declare x char(4); begin select ch into x from t694; raise notice '[%]', x; end $$": (
        "ok",
        ["[ab  ]"],
    ),
    "do $$ declare x numeric(6,1); begin select n into x from t694; raise notice '%', x; end $$": (
        "ok",
        ["12.4"],
    ),
    "do $$ declare x int; y dcc687; begin select k, d into x, y from t694;"
    " raise notice '% %', x, y; end $$": ("ok", ["1 ok"]),
    "do $$ declare x timestamp(0); begin select '2020-01-01 10:00:00.6'::timestamp"
    " into x; raise notice '%', x; end $$": ("ok", ["2020-01-01 10:00:01"]),
    "do $$ declare x int; begin select 5::int2 into x;"
    " raise notice '% %', x, pg_typeof(x); end $$": ("ok", ["5 integer"]),
}


@pytest.mark.backlog_fixed("RP-694")
@pytest.mark.parametrize("query", list(MOVED))
def test_a_moved_row_assigns_each_variable_by_the_assignment_rule(query: str, dsn: str) -> None:
    """Was: every error case stored its value unchecked (RP-694)."""
    assert _moved(dsn, query) == MOVED[query]


# An array's assignment coercion is an `ArrayCoerceExpr` whose element step
# is the element type's length coercion, by the same rule.
ARRAY_VARIABLE = {
    "do $$ declare x varchar(3)[]; begin x := array['abcdef']; end $$": _VARCHAR3,
    "do $$ declare x varchar(3)[] := array['abcdef']; begin end $$": _VARCHAR3,
}


@pytest.mark.backlog("RP-695")
@pytest.mark.parametrize("query", list(ARRAY_VARIABLE))
def test_array_variable_assignment_refuses_an_overlong_element(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ARRAY_VARIABLE[query]


# One `CoerceToDomain` checks the whole chain and names the domain it
# coerces to.
_DVV687 = ("error", "23514", 'value for domain dvv687 violates check constraint "dcc687_check"')
NESTED_DOMAIN = {
    "select 'no'::dvv687": _DVV687,
    "select 'no'::text::dvv687": _DVV687,
    "do $$ declare x dvv687; begin x := 'no'; end $$": _DVV687,
}


@pytest.mark.backlog("RP-696")
@pytest.mark.parametrize("query", list(NESTED_DOMAIN))
def test_cast_to_a_domain_over_a_domain_names_the_outer_domain(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == NESTED_DOMAIN[query]
