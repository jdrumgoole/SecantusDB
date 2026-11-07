"""Gap tests of the name-scope divergences found while working RP-443:
RP-451 (a parameter qualified by its function's name is not bound), RP-455
(a write's target is not an enclosing relation to its subqueries), RP-453
(a correlated subquery's value read from a CTE or a write's target came
out text; fixed), RP-454 (a join's alias is refused or not found), RP-456
(a CTE hidden by its alias still answers to its name in a subquery) and
RP-457 (an UPDATE ... FROM's or DELETE ... USING's RETURNING subquery
cannot read the FROM).

An answer is the rows, `("rowcount", n)` for a statement with none, or
`(sqlstate, message, hint, position)` for an error. The expected answers
were measured on a private, freshly initdb'd PostgreSQL 15.19 cluster with
`_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator
from decimal import Decimal
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists fe, fs cascade",
    "create table fe (a int, b text)",
    "create table fs (a int, c text)",
    "insert into fe values (1, 'x')",
    "insert into fs values (1, 'y')",
    "create or replace function fq1(p int) returns int language sql as $$ select fq1.p $$",
    "create or replace function fq2(p int) returns int language sql"
    " as $$ select (select fq2.p from fs limit 1) $$",
    "create or replace function fq3(p int) returns bool language sql"
    " as $$ select exists (select fq3.p from fs) $$",
    "create or replace function fq4(p int) returns int language plpgsql"
    " as $$ begin return (select fq4.p from fs limit 1); end $$",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])
        for f in ("fq1", "fq2", "fq3", "fq4"):
            c.execute(f"drop function if exists {f}(int)")


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
                e.diag.message_hint,
                e.diag.statement_position,
            )
        return cur.fetchall() if cur.description else ("rowcount", cur.rowcount)


# PostgreSQL resolves `fq1.p` as the parameter `p` of the function `fq1`
# (`sql_fn_post_column_ref` for SQL, `plpgsql_post_column_ref` for
# PL/pgSQL), at any depth of subquery.
FUNCTION_QUALIFIED_PARAMETER = {
    "select fq1(7)": [(7,)],
    "select fq2(7)": [(7,)],
    "select fq3(7)": [(True,)],
    "select fq4(7)": [(7,)],
}


@pytest.mark.backlog("RP-451")
@pytest.mark.parametrize("query", list(FUNCTION_QUALIFIED_PARAMETER))
def test_function_qualified_parameter(query: str, dsn: str) -> None:
    assert answer(dsn, query) == FUNCTION_QUALIFIED_PARAMETER[query]


_FE_ALIAS_X = (
    "42P01",
    'invalid reference to FROM-clause entry for table "fe"',
    'Perhaps you meant to reference the table alias "x".',
)

# An UPDATE's or DELETE's target is a relation of the statement's own
# level: a subquery's `fe.a` reads it, and with the target aliased `x` its
# table name is hidden.
WRITE_TARGET_SCOPE = {
    "update fe set a = (select fe.a from fe x limit 1)": ("rowcount", 1),
    "update fe x set a = (select fe.a from fs limit 1)": (*_FE_ALIAS_X, "29"),
    "delete from fe x where exists (select fe.a from fs)": (*_FE_ALIAS_X, "39"),
}


@pytest.mark.backlog("RP-455")
@pytest.mark.parametrize("query", list(WRITE_TARGET_SCOPE))
def test_write_target_scope_in_a_subquery(query: str, dsn: str) -> None:
    assert answer(dsn, query) == WRITE_TARGET_SCOPE[query]


# The subquery's value has the outer column's type, `integer`; it came out
# the text of an untyped parameter, as the outer reference's type was looked
# up only among tables and FROM-subqueries.
CORRELATED_VALUE_TYPE = {
    "with c as (select 1 as a) select (select c.a from fs limit 1) from c": [(1,)],
    "insert into fe values (2, 'x') returning (select fe.a from fs limit 1)": [(2,)],
    "update fe set b = 'z' returning (select fe.a from fs limit 1)": [(1,)],
    "delete from fe returning (select fe.a from fs limit 1)": [(1,)],
    "insert into fe as x values (3, 'y') returning (select x.a from fs limit 1)": [(3,)],
    "with c(x) as (select 1) select (select c.x from fs limit 1) from c": [(1,)],
    "with c as (select 2.5::numeric b, 1 a) select (select c.b from fs limit 1) from c": [
        (Decimal("2.5"),)
    ],
    "with c as (select 1::int8 a), d as (select a from c)"
    " select (select d.a from fs limit 1) from d": [(1,)],
    # An ARRAY sublink over it was `{NULL}`.
    "with c as (select 1 as a) select array(select c.a from fs) from c": [([1],)],
    # A CTE hides a table of its name: typed by the table, the text `q` was
    # sent as an integer.
    "with fs as (select 'q'::text as a) select (select fs.a from fe limit 1) from fs": [("q",)],
}


@pytest.mark.backlog_fixed("RP-453")
@pytest.mark.parametrize("query", list(CORRELATED_VALUE_TYPE))
def test_correlated_value_keeps_its_type(query: str, dsn: str) -> None:
    assert answer(dsn, query) == CORRELATED_VALUE_TYPE[query]


JOIN_ALIAS = {
    "select u.a from fs join fe using (a) as u": [(1,)],
    "select (select u.a from fs join fe using (a) as u limit 1)": [(1,)],
    "select j.a from (fs join fe using (a)) as j": [(1,)],
    "select (select j.a from (fs join fe using (a)) as j limit 1)": [(1,)],
}


@pytest.mark.backlog("RP-454")
@pytest.mark.parametrize("query", list(JOIN_ALIAS))
def test_join_alias(query: str, dsn: str) -> None:
    assert answer(dsn, query) == JOIN_ALIAS[query]


_C_ALIAS_X = (
    "42P01",
    'invalid reference to FROM-clause entry for table "c"',
    'Perhaps you meant to reference the table alias "x".',
)

# A CTE referenced under an alias is hidden by it, as a table is: a
# subquery's `c.a` names nothing.
CTE_ALIAS_HIDES_NAME = {
    "with c as (select 1 as a) select (select c.a from fs limit 1) from c x": (
        *_C_ALIAS_X,
        "42",
    ),
    "with c as (select 1 as a) select 1 from c x where exists (select c.a from fs)": (
        *_C_ALIAS_X,
        "66",
    ),
}


@pytest.mark.backlog("RP-456")
@pytest.mark.parametrize("query", list(CTE_ALIAS_HIDES_NAME))
def test_cte_alias_hides_its_name(query: str, dsn: str) -> None:
    assert answer(dsn, query) == CTE_ALIAS_HIDES_NAME[query]


# An UPDATE's FROM and a DELETE's USING are relations of the statement's
# own level, which its RETURNING's subqueries read.
RETURNING_READS_FROM = {
    "update fe set b = 'w' from fs returning (select fs.a from fe limit 1)": [(1,)],
    "delete from fe using fs returning (select fs.c from fe limit 1)": [("y",)],
    "update fe set b = 'w' from fs x returning (select x.c from fe limit 1)": [("y",)],
}


@pytest.mark.backlog("RP-457")
@pytest.mark.parametrize("query", list(RETURNING_READS_FROM))
def test_returning_subquery_reads_the_from(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RETURNING_READS_FROM[query]
