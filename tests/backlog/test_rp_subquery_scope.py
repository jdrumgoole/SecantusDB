"""Gap tests of the name-scope divergences found while working RP-443:
RP-451 (a parameter qualified by its function's name was not bound; fixed), RP-453
(a correlated subquery's value read from a CTE or a write's target came
out text; fixed), RP-454 (a join's alias was refused or not found;
fixed), RP-456 (a CTE hidden by its alias still answered to its name in a
subquery; fixed) and RP-457 (an UPDATE ... FROM's or DELETE ... USING's
RETURNING subquery could not read the FROM; fixed); the regression test of RP-455 (a
write's target was not an enclosing relation to its subqueries, and an
enclosing level's hidden table name was not `errorMissingRTE`'s "invalid
reference"); found while working RP-455, RP-460 (a subquery inside a
FROM-subquery is offered a sibling's alias as its hint); and found while
working RP-454, RP-469 (a FROM item named as a table an aliased JOIN hides
is refused), RP-470 (a whole-row reference to a joined table was not found;
fixed), RP-471 (EXPLAIN of an aliased JOIN), RP-467 (`pg_get_viewdef` of an
aliased JOIN) and RP-468 (a correlated subquery reading a join's column got
text; fixed); found while working RP-461, RP-478 (a LATERAL subquery's bare
name for an earlier FROM item's column was not found; fixed); and found while
working RP-470, RP-479 (a whole-row reference still refused in three
shapes), RP-480 (a whole row was typed as a record or as varchar; fixed)
and RP-481 (a USING column's 42703 carries a position).

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
# own level, which its RETURNING's subqueries read. Each was 42P01: only a
# RETURNING item naming a FROM item outside any subquery was read through
# the FROM.
RETURNING_READS_FROM = {
    "update fe set b = 'w' from fs returning (select fs.a from fe limit 1)": [(1,)],
    "delete from fe using fs returning (select fs.c from fe limit 1)": [("y",)],
    "update fe set b = 'w' from fs x returning (select x.c from fe limit 1)": [("y",)],
}


@pytest.mark.backlog_fixed("RP-457")
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
# (or, for `fe.*` cast, 42P01 invalid reference). A table's or subquery's
# row is built at its scan, so the NULL-extended side of an outer join has
# a NULL row, not a row of NULLs; an aliased JOIN's (or a USING alias's)
# is a record of its columns, NULLs and all.
WHOLE_ROW_OVER_A_JOIN = {
    "select fs from fs, fe": [("(1,y)",)],
    "select fs from fs join fe using (a)": [("(1,y)",)],
    "select fe.*::text from fs join fe using (a)": [("(1,x)",)],
    "select j from (fs join fe using (a)) as j": [(("1", "y", "x"),)],
    "select fe from fs left join fe on false": [(None,)],
    "select s from fs left join (select a, b from fe) s on false": [(None,)],
    "select count(fe) from fs left join fe on false": [(0,)],
    "select j from fe left join (fs join fe as f2 using (a)) as j on false": [
        ((None, None, None),)
    ],
    "select u from fs join fe using (a) as u": [(("1",),)],
    "select (fs).c, (fs.*).c from fs join fe using (a)": [("y", "y")],
    "select fe::text from fs join fe using (a) order by 1": [("(1,x)",)],
    "select fs.a from fs join fe on fe = row(1, 'x')::fe": [(1,)],
    "select row(fs.*, fe.b) from fs join fe using (a)": [(("1", "y", "x"),)],
    "select s from fe, (select 1 as x, 2 as x) s": [(("1", "2"),)],
}


@pytest.mark.backlog_fixed("RP-470")
@pytest.mark.parametrize("query", list(WHOLE_ROW_OVER_A_JOIN))
def test_whole_row_over_a_join(query: str, dsn: str) -> None:
    assert answer(dsn, query) == WHOLE_ROW_OVER_A_JOIN[query]


@pytest.mark.backlog_fixed("RP-470")
def test_whole_row_output_names(dsn: str) -> None:
    """A cast whole row is named after the FROM item (FigureColname), not
    `row`: it was over a join, and over one table for `t::text`."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        cur = c.execute("select fe.*::text, fe::text, (fs.*)::text from fs join fe using (a)")
        names = [d.name for d in cur.description]
        lone = c.execute("select fe::text, (fe.*)::text from fe").description
    assert names == ["fe", "fe", "fs"]
    assert [d.name for d in lone] == ["fe", "fe"]


# Found while working RP-470. A LATERAL subquery's alias, a whole row named
# inside a subquery, and `(t).*` under an ORDER BY of its position.
WHOLE_ROW_REMAINDER = {
    "select l from fs join lateral (select fs.a) l on true": [(("1",),)],
    "select l from fe left join lateral (select fe.a from fs where false) l on true": [(None,)],
    "select (select fs) from fs, fe": [("(1,y)",)],
    "select (select fs) from fs": [("(1,y)",)],
    "select (fs).* from fs, fe order by 1": [(1, "y")],
}


@pytest.mark.backlog("RP-479")
@pytest.mark.parametrize("query", list(WHOLE_ROW_REMAINDER))
def test_whole_row_remainder(query: str, dsn: str) -> None:
    assert answer(dsn, query) == WHOLE_ROW_REMAINDER[query]


# Found while working RP-470. A table's whole row is of its row type even
# under a column-alias list; psycopg reads an unknown composite as text and
# a record as a tuple, which is what it read here before RP-480 was fixed.
WHOLE_ROW_COLUMN_ALIASES = {
    "select x from fs as x(p, q)": [("(1,y)",)],
    "select x from fs as x(p, q), fe": [("(1,y)",)],
    "select x from fe left join fs as x(p, q) on false": [(None,)],
}


@pytest.mark.backlog_fixed("RP-480")
@pytest.mark.parametrize("query", list(WHOLE_ROW_COLUMN_ALIASES))
def test_whole_row_under_column_aliases(query: str, dsn: str) -> None:
    assert answer(dsn, query) == WHOLE_ROW_COLUMN_ALIASES[query]


# Found while working RP-480. Under a column-alias list the row keeps the
# relation's own field names; here `row_to_json` printed the aliases.
WHOLE_ROW_FIELD_NAMES = {
    "select row_to_json(x) from fs as x(p, q)": [({"a": 1, "c": "y"},)],
    "select row_to_json(x) from fs as x(p, q), fe": [({"a": 1, "c": "y"},)],
    "select (x).c, (x).p from fs as x(p)": [("y", 1)],
}


@pytest.mark.backlog_fixed("RP-480")
@pytest.mark.parametrize("query", list(WHOLE_ROW_FIELD_NAMES))
def test_whole_row_under_column_aliases_keeps_field_names(query: str, dsn: str) -> None:
    assert answer(dsn, query) == WHOLE_ROW_FIELD_NAMES[query]


@pytest.mark.backlog_fixed("RP-480")
def test_whole_row_of_a_view_is_its_row_type(dsn: str) -> None:
    """A view's whole row is of the view's row type (the RowDescription's
    type OID is `'v'::regtype`), alone, over a join or under a column-alias
    list; here it was 1043, varchar."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.execute("create view rp_wv as select a, c from fs")
        try:
            lone = c.execute("select rp_wv from rp_wv").description[0].type_code
            joined = c.execute("select rp_wv from rp_wv, fe").description[0].type_code
            aliased = c.execute("select v from rp_wv as v(p, q)").description[0].type_code
            row = c.execute("select 'rp_wv'::regtype::oid").fetchone()
        finally:
            c.execute("drop view rp_wv")
    assert row is not None and (lone, joined, aliased) == (row[0], row[0], row[0])


# Found while working RP-470. PostgreSQL 15.19 reports a missing USING
# column's 42703 with no position; here it points into the statement.
USING_COLUMN_MISSING = {
    "select * from fs as x(p) join fe using (a)": (
        "42703",
        'column "a" specified in USING clause does not exist in left table',
        None,
        None,
    ),
    "select * from fs join fe using (nosuch)": (
        "42703",
        'column "nosuch" specified in USING clause does not exist in left table',
        None,
        None,
    ),
}


@pytest.mark.backlog("RP-481")
@pytest.mark.parametrize("query", list(USING_COLUMN_MISSING))
def test_using_column_missing_has_no_position(query: str, dsn: str) -> None:
    assert answer(dsn, query) == USING_COLUMN_MISSING[query]


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
# column's type: a leaf's own, a USING join's merged one (the sides' common
# type, `bigint` over int4 and int8), or one of the join's alias. It came
# out text, as RP-453's lookup of the outer reference's type did not reach
# into a join. A CTE leaf is the CTE, ahead of the table of its name.
JOIN_COLUMN_IN_A_SUBQUERY = {
    "select (select fs.a from fe where fe.a = fs.a) from fs join fe using (a)": [(1,)],
    "select (select j.a from fe limit 1) from (fs join fe using (a)) as j": [(1,)],
    "select (select u.a from fe limit 1) from fs join fe using (a) as u": [(1,)],
    "select (select u.a from fe limit 1)"
    " from fs join (select 1::int8 a, 2.5::numeric d) g using (a) as u": [(1,)],
    "select (select g.d from fe limit 1)"
    " from fs join (select 1::int8 a, 2.5::numeric d) g using (a)": [(Decimal("2.5"),)],
    "select (select j.x from fe limit 1) from (fs join fe using (a)) as j(x, y, z)": [(1,)],
    "with fs as (select 'q'::text as a)"
    " select (select fs.a from fe limit 1) from fs join fe on true": [("q",)],
    "select (select fs.a * 2 from fe limit 1) from fs cross join fe": [(2,)],
    "select (select fe.a from fs limit 1) from fs join (fe join fs x using (a)) using (a)": [(1,)],
}


@pytest.mark.backlog_fixed("RP-468")
@pytest.mark.parametrize("query", list(JOIN_COLUMN_IN_A_SUBQUERY))
def test_join_column_in_a_subquery_keeps_its_type(query: str, dsn: str) -> None:
    assert answer(dsn, query) == JOIN_COLUMN_IN_A_SUBQUERY[query]


# Found while working RP-461: a LATERAL subquery reads the FROM items
# before it by their bare column names too, after its own levels'
# (`colNameToVar` searches innermost first), and an aggregate over only
# those names belongs to the level whose FROM it sits in (42803). Only a
# qualified name (`fe.b`) was found, and a bare one was 42703.
LATERAL_BARE_NAME = {
    "select x from fe, lateral (select b as x) s": [("x",)],
    "select x from fe join lateral (select b || c as x from fs) s on true": [("xy",)],
    # In a subquery inside it, at any depth.
    "select x from fe, lateral (select (select b) as x from fs) s": [("x",)],
    "select x from fe, lateral (select (select (select b)) as x) s": [("x",)],
    "select x from fe, lateral (select x from (select b as x) d) s": [("x",)],
    "select c from fe, lateral (select c from fs where b = 'x') s": [("y",)],
    "select x from fe, lateral (select b || g as x from generate_series(1, 2) g) s order by x": [
        ("x1",),
        ("x2",),
    ],
    "select x from fe, lateral (select b as x union all select c from fs) s order by x": [
        ("x",),
        ("y",),
    ],
    # A nearer level's own column of that name wins.
    "select x from fe, lateral (select b || q as x from (select 'w' as q) d) s": [("xw",)],
    "select x from fe, lateral (select b as x from (select 'w' as b) d) s": [("w",)],
    "select x from fe, lateral (select b as x from fs t(b)) s": [(1,)],
    # Two left items with the name, or none.
    "select * from fe, fs, lateral (select a) s": (
        "42702",
        'column reference "a" is ambiguous',
        None,
        "39",
    ),
    "select * from fe, lateral (select zz) s": ("42703", 'column "zz" does not exist', None, "35"),
    # An aggregate over only the left side's columns.
    "select x from fe, lateral (select string_agg(b, ',') as x from generate_series(1, 2)) s": (
        "42803",
        "aggregate functions are not allowed in FROM clause of their own query level",
        None,
        "35",
    ),
    "select x from fe, lateral (select string_agg(b, ',') as x from generate_series(1, a)) s": (
        "42803",
        "aggregate functions are not allowed in FROM clause of their own query level",
        None,
        "35",
    ),
}


@pytest.mark.backlog_fixed("RP-478")
@pytest.mark.parametrize("query", list(LATERAL_BARE_NAME))
def test_lateral_subquery_reads_an_earlier_item_by_bare_name(query: str, dsn: str) -> None:
    assert answer(dsn, query) == LATERAL_BARE_NAME[query]


# Found while working RP-478: a LATERAL subquery was planned once over
# sample values for its output's shape, and that planning evaluated a
# FROM-less body's targets, so a `nextval` in one was drawn once more than
# the left side has rows. The qualified form (`fe.a`) and the bare one
# (`a`, since RP-478) alike.
LATERAL_DRAWS = [
    "select a, n from fe, lateral (select nextval('lq') + fe.a as n) s",
    "select a, n from fe, lateral (select nextval('lq') + a as n) s",
]


@pytest.mark.backlog_fixed("RP-482")
@pytest.mark.parametrize("query", LATERAL_DRAWS)
def test_lateral_subquery_draws_once_per_row(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.execute("drop sequence if exists lq")
        c.execute("create sequence lq")
        try:
            rows = c.execute(query).fetchall()
            drawn = c.execute("select last_value from lq").fetchall()
        finally:
            c.execute("drop sequence lq")
    assert (rows, drawn) == ([(1, 2)], [(1,)])


# The same shape planning called a user function in a LATERAL item once
# more than the left side has rows: a function in FROM (implicitly
# lateral), a LATERAL one, and one called in a LATERAL subquery's select
# list each wrote its row twice for fe's one. PostgreSQL 15.19, on a
# private cluster: once each.
LATERAL_CALLS = {
    "select * from fe, lateral lz(fe.a)": [(1, "x", 1)],
    "select * from fe, lz(a)": [(1, "x", 1)],
    "select * from fe, lateral ly(fe.a)": [(1, "x", 1)],
    "select a, n from fe, lateral (select ly(fe.a) as n) s": [(1, 1)],
    "select a, n from fe, lateral (select x + 1 as n from lz(fe.a) x) s": [(1, 2)],
}


@pytest.mark.backlog_fixed("RP-482")
@pytest.mark.parametrize("query", list(LATERAL_CALLS))
def test_lateral_item_calls_once_per_row(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.execute("drop table if exists lcalls")
        c.execute("create table lcalls (x int)")
        c.execute(
            "create or replace function lz(i int) returns setof int language sql"
            " as $$ insert into lcalls values (i); select i $$"
        )
        c.execute(
            "create or replace function ly(i int) returns int language sql"
            " as $$ insert into lcalls values (i); select i $$"
        )
        try:
            rows = c.execute(query).fetchall()
            calls = c.execute("select count(*) from lcalls").fetchall()
        finally:
            c.execute("drop function lz(int)")
            c.execute("drop function ly(int)")
            c.execute("drop table lcalls")
    assert (rows, calls) == (LATERAL_CALLS[query], [(1,)])
