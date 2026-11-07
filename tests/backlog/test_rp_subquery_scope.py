"""Gap tests of the name-scope divergences found while working RP-443:
RP-451 (a parameter qualified by its function's name was not bound; fixed), RP-453
(a correlated subquery's value read from a CTE or a write's target came
out text; fixed), RP-454 (a join's alias was refused or not found;
fixed), RP-456 (a CTE hidden by its alias still answered to its name in a
subquery; fixed) and RP-457 (an UPDATE ... FROM's or DELETE ... USING's
RETURNING subquery cannot read the FROM); the regression test of RP-455 (a
write's target was not an enclosing relation to its subqueries, and an
enclosing level's hidden table name was not `errorMissingRTE`'s "invalid
reference"); found while working RP-455, RP-460 (a subquery inside a
FROM-subquery is offered a sibling's alias as its hint); and found while
working RP-454, RP-469 (a FROM item named as a table an aliased JOIN hides
is refused), RP-470 (a whole-row reference to a joined table), RP-471
(EXPLAIN of an aliased JOIN), RP-467 (`pg_get_viewdef` of an aliased JOIN)
and RP-468 (a correlated subquery reading a join's column gets text).

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
# PL/pgSQL), at any depth of subquery. Each was refused: a dotted name was
# skipped by the binder, and taken for a table.
FUNCTION_QUALIFIED_PARAMETER = {
    "select fq1(7)": [(7,)],
    "select fq2(7)": [(7,)],
    "select fq3(7)": [(True,)],
    "select fq4(7)": [(7,)],
}


@pytest.mark.backlog_fixed("RP-451")
@pytest.mark.parametrize("query", list(FUNCTION_QUALIFIED_PARAMETER))
def test_function_qualified_parameter(query: str, dsn: str) -> None:
    assert answer(dsn, query) == FUNCTION_QUALIFIED_PARAMETER[query]


_FE_ALIAS_X = (
    "42P01",
    'invalid reference to FROM-clause entry for table "fe"',
    'Perhaps you meant to reference the table alias "x".',
)


def _fe_alias_x(position: str) -> tuple[str, ...]:
    return (*_FE_ALIAS_X, position)


def _entry(table: str, eref: str, position: str) -> tuple[str, ...]:
    """`errorMissingRTE`'s error for an entry that is not an alias's."""
    return (
        "42P01",
        f'invalid reference to FROM-clause entry for table "{table}"',
        f'There is an entry for table "{eref}", but it cannot be referenced'
        " from this part of the query.",
        position,
    )


_FS_ALIAS_Y = (
    "42P01",
    'invalid reference to FROM-clause entry for table "fs"',
    'Perhaps you meant to reference the table alias "y".',
)

# An UPDATE's or DELETE's target is a relation of the statement's own
# level: a subquery's `fe.a` reads it, and with the target aliased `x` its
# table name is hidden -- in a SET, a WHERE or a RETURNING, and so is an
# aliased table of an UPDATE's FROM or a DELETE's USING. An INSERT's target
# is in its level's range table but not its namespace, except for RETURNING.
WRITE_TARGET_SCOPE = {
    "update fe set a = (select fe.a from fe x limit 1)": ("rowcount", 1),
    "update fe x set a = (select fe.a from fs limit 1)": _fe_alias_x("29"),
    "delete from fe x where exists (select fe.a from fs)": _fe_alias_x("39"),
    "update fe x set a = 3 where (select count(*) from fs where fs.a = fe.a) = 1": (
        _fe_alias_x("67")
    ),
    "update fe x set a = 1 returning (select fe.a from fs limit 1)": _fe_alias_x("41"),
    "delete from fe x returning (select fe.a from fs limit 1)": _fe_alias_x("36"),
    "delete from fe x where x.a = (select max(fe.a) from fs)": _fe_alias_x("42"),
    "delete from fe using fs y where exists (select fs.a from fe x)": (*_FS_ALIAS_Y, "48"),
    "update fe set a = 3 from fs y where exists (select fs.a)": (*_FS_ALIAS_Y, "52"),
    "delete from fe where exists (select fe.a from fe x)": ("rowcount", 1),
    "update fe x set a = (select x.a + 1 from fs limit 1)": ("rowcount", 1),
    "update fe x set a = 7 where exists (select x.a from fs where fs.a = x.a)": ("rowcount", 1),
    # The subquery's own `x` shadows the target's alias, so the entry found
    # for `fe` (the target) is named by its eref, not offered as an alias.
    "update fe x set a = (select fe.a from fs x limit 1)": _entry("fe", "x", "29"),
    "insert into fe as x values (2, 'z') returning (select fe.a from fs limit 1)": (
        _fe_alias_x("55")
    ),
    "insert into fe select (select fe.a from fs limit 1), 'z'": _entry("fe", "fe", "31"),
}


@pytest.mark.backlog_fixed("RP-455")
@pytest.mark.parametrize("query", list(WRITE_TARGET_SCOPE))
def test_write_target_scope_in_a_subquery(query: str, dsn: str) -> None:
    assert answer(dsn, query) == WRITE_TARGET_SCOPE[query]


# The same `errorMissingRTE` for a SELECT's enclosing level, which an EXISTS
# (whose select list is dropped) and a join's ON used to step past.
ENCLOSING_HIDDEN_NAME = {
    "select 1 from fe x where exists (select fe.a from fs)": _fe_alias_x("41"),
    "select 1 from fe x where exists (select 1 from fs where exists (select fe.a))": (
        _fe_alias_x("72")
    ),
    "select 1 from fe x where 1 = any (select fe.a from fs)": _fe_alias_x("42"),
    "select 1 from fe x, fs y where exists (select fs.a)": (*_FS_ALIAS_Y, "47"),
    "select * from fe x join fs on exists (select fe.a)": _fe_alias_x("46"),
    "select * from fe x, lateral (select (select fe.a)) s": _fe_alias_x("45"),
    "with c as (select 1 a) select 1 from c x where exists (select c.a from fs)": (
        "42P01",
        'invalid reference to FROM-clause entry for table "c"',
        'Perhaps you meant to reference the table alias "x".',
        "63",
    ),
}


@pytest.mark.backlog_fixed("RP-455")
@pytest.mark.parametrize("query", list(ENCLOSING_HIDDEN_NAME))
def test_enclosing_hidden_name_in_a_subquery(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ENCLOSING_HIDDEN_NAME[query]


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


_FS_HIDDEN = (
    "42P01",
    'invalid reference to FROM-clause entry for table "fs"',
    'There is an entry for table "fs", but it cannot be referenced from this part of the query.',
)

# A join's alias (`(fs join fe using (a)) as j`) names the join's columns,
# the ones `*` lists, renamed by a column list; it hides the sides' names.
# A USING join's alias (`using (a) as u`, PostgreSQL 14) names its merged
# columns only, beside the sides' names. The first was 0A000 `an aliased
# JOIN is not supported yet`, the second 42P01 `missing FROM-clause entry
# for table "u"`, and inside a subquery a 42883 naming the correlated
# rewrite's internal function.
JOIN_ALIAS = {
    "select u.a from fs join fe using (a) as u": [(1,)],
    "select (select u.a from fs join fe using (a) as u limit 1)": [(1,)],
    "select j.a from (fs join fe using (a)) as j": [(1,)],
    "select (select j.a from (fs join fe using (a)) as j limit 1)": [(1,)],
    "select * from (fs join fe using (a)) as j": [(1, "y", "x")],
    "select j.* from (fs join fe on fs.a = fe.a) as j": [(1, "y", 1, "x")],
    "select j.x, j.y, j.b from (fs join fe using (a)) as j(x, y)": [(1, "y", "x")],
    "select * from (fs join fe using (a)) as j(x, y)": [(1, "y", "x")],
    "select u.* from fs join fe using (a) as u": [(1,)],
    "select fs.c, fe.b, u.a from fs join fe using (a) as u": [("y", "x", 1)],
    "select u.c from fs join fe using (a) as u": (
        "42703",
        "column u.c does not exist",
        'Perhaps you meant to reference the column "fs.c".',
        "8",
    ),
    "select fs.a from (fs join fe using (a)) as j": (*_FS_HIDDEN, "8"),
    "select j.a from (fs join fe on fs.a = fe.a) as j": (
        "42702",
        'column reference "a" is ambiguous',
        None,
        "8",
    ),
    "select * from (fs join fe using (a)) as j(x, y, z, w)": (
        "42P10",
        'join expression "j" has 3 columns available but 4 columns specified',
        None,
        None,
    ),
    # The operator check sees the join's column through its alias; the
    # comparison across types matched nothing and answered no rows.
    "select * from (fs join fe using (a)) as j where j.b = 1": (
        "42883",
        "operator does not exist: text = integer",
        "No operator matches the given name and argument types."
        " You might need to add explicit type casts.",
        "53",
    ),
    "select * from fs join fe using (a) as u where u.a = 'q'": (
        "22P02",
        'invalid input syntax for type integer: "q"',
        None,
        "53",
    ),
    "select * from fs join fe using (a) as fs": (
        "42712",
        'table name "fs" specified more than once',
        None,
        None,
    ),
    "select u.a from (fs join fe using (a) as u) as x": (
        "42P01",
        'missing FROM-clause entry for table "u"',
        None,
        "8",
    ),
    # A nested USING join's merged column is visible in an enclosing ON;
    # it was dropped from it, so a bare `a` read `x.a` alone.
    "select * from fs join fe using (a) join fe x on a = x.a": (
        "42702",
        'column reference "a" is ambiguous',
        None,
        "49",
    ),
    "select j.c from (fs join fe using (a)) as j join fe x on j.a = x.a": [("y",)],
    "select j.c from (fs join fe using (a)) as j join fe x on fs.a = x.a": (*_FS_HIDDEN, "58"),
    "select exists (select j.c from (fs join fe using (a)) as j)": [(True,)],
    "select exists (select u.a from fs join fe using (a) as u)": [(True,)],
    # A name the alias hides is the enclosing query's, when it has one.
    "select (select fs.a from (fs join fe using (a)) as j) from fs": [(1,)],
    "select (select fs.a from (fs join fe using (a)) as j)": (*_FS_HIDDEN, "16"),
    "select * from fe where exists (select from (fs join fe x using (a)) as j where j.b = fe.b)": [
        (1, "x")
    ],
    "select j.c from (fs left join fe on fs.a = fe.a + 1) as j where j.b is null": [("y",)],
    "select k.c, k.b from ((fs join fe using (a)) as j join fe x using (a)) as k": (
        "42702",
        'column reference "b" is ambiguous',
        None,
        "13",
    ),
}


@pytest.mark.backlog_fixed("RP-454")
@pytest.mark.parametrize("query", list(JOIN_ALIAS))
def test_join_alias(query: str, dsn: str) -> None:
    assert answer(dsn, query) == JOIN_ALIAS[query]


_C_ALIAS_X = (
    "42P01",
    'invalid reference to FROM-clause entry for table "c"',
    'Perhaps you meant to reference the table alias "x".',
)

# A CTE referenced under an alias is hidden by it, as a table is: a
# subquery's `c.a` is `errorMissingRTE`'s "invalid reference" with the
# alias hint. It was `missing FROM-clause entry` at the WITH's own name,
# and the EXISTS form answered its row (fixed with RP-455's range-table
# search).
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


@pytest.mark.backlog_fixed("RP-456")
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


# A FROM-subquery that is not LATERAL is analysed with the FROM items before
# it in its level's range table but out of its namespace (`p_lateral_only`),
# so a subquery inside it that names an aliased one by its table's name is
# `errorMissingRTE`'s "cannot be referenced from this part of the query":
# the alias is not visible there, so it is not offered as the hint.
FROM_SUBQUERY_SIBLING = {
    "select * from fe x, (select (select fe.a)) s": _entry("fe", "x", "37"),
    "select * from fe x, (select 1 where exists (select fe.a)) s": _entry("fe", "x", "52"),
}


@pytest.mark.backlog("RP-460")
@pytest.mark.parametrize("query", list(FROM_SUBQUERY_SIBLING))
def test_from_subquery_sibling_is_not_visible(query: str, dsn: str) -> None:
    assert answer(dsn, query) == FROM_SUBQUERY_SIBLING[query]


# Found while working RP-454. An aliased JOIN hides its sides' names, so
# another FROM item may reuse one; here the two leaves' keys would be one
# (a joined row is keyed by the table's alias), and it is refused by name.
HIDDEN_NAME_REUSED = {
    "select * from (fs join fe using (a)) as j, fs": [(1, "y", "x", 1, "y")],
    "select * from fs, (fs join fe using (a)) as j": [(1, "y", 1, "y", "x")],
}


@pytest.mark.backlog("RP-469")
@pytest.mark.parametrize("query", list(HIDDEN_NAME_REUSED))
def test_hidden_join_name_reused(query: str, dsn: str) -> None:
    assert answer(dsn, query) == HIDDEN_NAME_REUSED[query]


# A FROM item's name alone is its whole row; over a join it was 42703
# (or, for `fe.*` cast, 42P01 invalid reference).
WHOLE_ROW_OVER_A_JOIN = {
    "select fs from fs, fe": [("(1,y)",)],
    "select fs from fs join fe using (a)": [("(1,y)",)],
    "select fe.*::text from fs join fe using (a)": [("(1,x)",)],
    "select j from (fs join fe using (a)) as j": [(("1", "y", "x"),)],
}


@pytest.mark.backlog("RP-470")
@pytest.mark.parametrize("query", list(WHOLE_ROW_OVER_A_JOIN))
def test_whole_row_over_a_join(query: str, dsn: str) -> None:
    assert answer(dsn, query) == WHOLE_ROW_OVER_A_JOIN[query]


_JOIN_PLAN = [
    ("Hash Join",),
    ("  Hash Cond: (fs.a = fe.a)",),
    ("  ->  Seq Scan on fs",),
    ("  ->  Hash",),
    ("        ->  Seq Scan on fe",),
    ("              Filter: (b = 'x'::text)",),
]

# PostgreSQL flattens a join alias's columns to the sides' own
# (`flatten_join_alias_vars`), so `j.b = 'x'` is `fe`'s scan filter and the
# plan is the unaliased one's. Here the aliased form prints no Hash Cond
# and keeps the conjunct above the join.
ALIASED_JOIN_PLAN = {
    "explain (costs off) select fs.c from fs join fe on fs.a = fe.a where fe.b = 'x'": _JOIN_PLAN,
    "explain (costs off) select j.c from (fs join fe on fs.a = fe.a) as j where j.b = 'x'": (
        _JOIN_PLAN
    ),
}


@pytest.mark.backlog("RP-471")
@pytest.mark.parametrize("query", list(ALIASED_JOIN_PLAN))
def test_aliased_join_plan(query: str, dsn: str) -> None:
    assert answer(dsn, query) == ALIASED_JOIN_PLAN[query]


# ruleutils prints an aliased JOIN with its alias (and a column list when
# the join's names repeat), and a USING alias's column by the left side's
# name; here the view's own text comes back.
ALIASED_JOIN_VIEWDEF = {
    "select j.c from (fs join fe using (a)) as j": (
        " SELECT j.c\n   FROM (fs\n     JOIN fe USING (a)) j;"
    ),
    "select u.a from fs join fe using (a) as u": (
        " SELECT fs.a\n   FROM (fs\n     JOIN fe USING (a) AS u);"
    ),
    "select j.c from (fs join fe on fs.a = fe.a) as j": (
        " SELECT j.c\n   FROM (fs\n     JOIN fe ON ((fs.a = fe.a))) j(a, c, a_1, b);"
    ),
}


@pytest.mark.backlog("RP-467")
@pytest.mark.parametrize("query", list(ALIASED_JOIN_VIEWDEF))
def test_aliased_join_viewdef(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.execute("create view rp_v as " + query)
        try:
            row = c.execute("select pg_get_viewdef('rp_v')").fetchone()
        finally:
            c.execute("drop view rp_v")
    assert row is not None and row[0] == ALIASED_JOIN_VIEWDEF[query]


# A correlated subquery's value read from a join's column has that
# column's type, `integer`; it came out text, as RP-453's lookup of the
# outer reference's type does not reach into a join.
JOIN_COLUMN_IN_A_SUBQUERY = {
    "select (select fs.a from fe where fe.a = fs.a) from fs join fe using (a)": [(1,)],
    "select (select j.a from fe limit 1) from (fs join fe using (a)) as j": [(1,)],
    "select (select u.a from fe limit 1) from fs join fe using (a) as u": [(1,)],
}


@pytest.mark.backlog("RP-468")
@pytest.mark.parametrize("query", list(JOIN_COLUMN_IN_A_SUBQUERY))
def test_join_column_in_a_subquery_keeps_its_type(query: str, dsn: str) -> None:
    assert answer(dsn, query) == JOIN_COLUMN_IN_A_SUBQUERY[query]
