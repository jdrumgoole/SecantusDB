"""Gap tests of the missing-column and ON-clause errors found while working
RP-408: RP-413 (a function in FROM leaves a 42703 with no HINT), RP-414 (a
42703 names the wrong reference or drops its qualifier), RP-415 (a column
in an INSERT's VALUES was refused as a gap; fixed) and RP-416 (a join's ON
after a comma item was not type-checked; fixed); RP-430, found while fixing
RP-416 (a subquery's join ON could not name the outer query; fixed); and
RP-443, found while fixing RP-415 (a subquery's qualifier naming no FROM
entry is not 42P01).

An answer is `(sqlstate, message, hint, position)`, or `None` for a
statement that succeeds. The expected answers were measured on a private,
freshly initdb'd PostgreSQL 15.19 cluster with the tables `_SETUP` creates.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists e, s, k, g",
    "create table e (a int, b text)",
    "create table s (a int, c text)",
    "create table k (a int, b text, bc int)",
    "create table g (a int, b int generated always as (a * 2) stored)",
]

Answer = tuple[str | None, str | None, str | None, str | None] | None


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
    yield dsn
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(_SETUP[0])


def answer(dsn: str, query: str) -> Answer:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )
    return None


FUNCTION_IN_FROM = {
    "select * from generate_series(1,2) g where gg = 1": (
        "42703",
        'column "gg" does not exist',
        'Perhaps you meant to reference the column "g.g".',
        "44",
    ),
    "select * from e, generate_series(1, 2) g where bb = 1": (
        "42703",
        'column "bb" does not exist',
        'Perhaps you meant to reference the column "e.b".',
        "48",
    ),
}


@pytest.mark.backlog("RP-413")
@pytest.mark.parametrize("query", list(FUNCTION_IN_FROM))
def test_function_in_from_hint(query: str, dsn: str) -> None:
    assert answer(dsn, query) == FUNCTION_IN_FROM[query]


AS_WRITTEN = {
    # The first missing column in analysis order, not the last.
    "select * from e, k where k.bcc = 1 and e.bb = 2": (
        "42703",
        "column k.bcc does not exist",
        'Perhaps you meant to reference the column "k.bc".',
        "26",
    ),
    # A third table's qualifier.
    "select * from e join s on e.a = s.a join k on k.bcc = 1": (
        "42703",
        "column k.bcc does not exist",
        'Perhaps you meant to reference the column "k.bc".',
        "47",
    ),
    # An UPDATE's or DELETE's alias.
    "update e x set a = 1 where x.bb = 1": (
        "42703",
        "column x.bb does not exist",
        'Perhaps you meant to reference the column "x.b".',
        "28",
    ),
    "update e x set a = x.bb": (
        "42703",
        "column x.bb does not exist",
        'Perhaps you meant to reference the column "x.b".',
        "20",
    ),
    "delete from e x where x.bb = 1": (
        "42703",
        "column x.bb does not exist",
        'Perhaps you meant to reference the column "x.b".',
        "23",
    ),
    # A schema-qualified reference, positioned at its first token.
    "select public.e.bb from e": (
        "42703",
        "column e.bb does not exist",
        'Perhaps you meant to reference the column "e.b".',
        "8",
    ),
    # ON CONFLICT's `excluded`, by its own name.
    "insert into e values (1, 'x') on conflict (a) do update set b = excluded.bb": (
        "42703",
        "column excluded.bb does not exist",
        'Perhaps you meant to reference the column "excluded.b".',
        "65",
    ),
}


@pytest.mark.backlog("RP-414")
@pytest.mark.parametrize("query", list(AS_WRITTEN))
def test_missing_column_reported_as_written(query: str, dsn: str) -> None:
    assert answer(dsn, query) == AS_WRITTEN[query]


INSERT_VALUES = {
    "insert into e values (bb)": (
        "42703",
        'column "bb" does not exist',
        'Perhaps you meant to reference the column "e.b".',
        "23",
    ),
    "insert into e values (1, 'x'), (bb, 'y')": (
        "42703",
        'column "bb" does not exist',
        'Perhaps you meant to reference the column "e.b".',
        "33",
    ),
}


@pytest.mark.backlog_fixed("RP-415")
@pytest.mark.parametrize("query", list(INSERT_VALUES))
def test_insert_values_column_reference(query: str, dsn: str) -> None:
    """A column reference in an INSERT's VALUES was refused as a gap,
    `0A000 ColumnRef is not supported yet`; PostgreSQL analyses VALUES in
    the INSERT's own level, where the target is in the range table but not
    visible, and answers 42703 with errorMissingColumn's hint."""
    assert answer(dsn, query) == INSERT_VALUES[query]


_NOT_HERE = (
    'There is an entry for table "{}", but it cannot be referenced from this part of the query.'
)


def _invalid(table: str, entry: str, position: str) -> Answer:
    return (
        "42P01",
        f'invalid reference to FROM-clause entry for table "{table}"',
        _NOT_HERE.format(entry),
        position,
    )


def _not_visible(column: str, position: str) -> Answer:
    return (
        "42703",
        f'column "{column}" does not exist',
        f'There is a column named "{column}" in table "e", but it cannot be'
        " referenced from this part of the query.",
        position,
    )


_BB_HINT = 'Perhaps you meant to reference the column "e.b".'

INSERT_VALUES_FORMS = {
    # A name the target has: in the range table, out of reach.
    "insert into e values (a)": _not_visible("a", "23"),
    "insert into e (b) values (a)": _not_visible("a", "27"),
    "insert into e values (a + 1, 'x')": _not_visible("a", "23"),
    "insert into e values (1, upper(b))": _not_visible("b", "32"),
    "insert into e values (1, coalesce(b, 'x'))": _not_visible("b", "35"),
    "insert into e values (1, 'x' || b)": _not_visible("b", "33"),
    "insert into e values (case when a > 1 then 1 end, 'x')": _not_visible("a", "33"),
    "insert into e values (default, b)": _not_visible("b", "32"),
    "insert into e values (nosuch, 'x', 3)": (
        "42703",
        'column "nosuch" does not exist',
        None,
        "23",
    ),
    "insert into e values (e)": ("42703", 'column "e" does not exist', None, "23"),
    "insert into e values ((zz).f)": ("42703", 'column "zz" does not exist', None, "24"),
    "insert into e values (zz[1])": ("42703", 'column "zz" does not exist', None, "23"),
    "insert into e values (1::text::int + zz, 'x')": (
        "42703",
        'column "zz" does not exist',
        None,
        "38",
    ),
    "insert into e as x values (bb)": (
        "42703",
        'column "bb" does not exist',
        'Perhaps you meant to reference the column "x.b".',
        "28",
    ),
    # A qualifier naming the target, by its name, its alias or its schema.
    "insert into e values (e.a)": _invalid("e", "e", "23"),
    "insert into e values (e.bb)": _invalid("e", "e", "23"),
    "insert into e values (public.e.a)": _invalid("e", "e", "23"),
    "insert into e values (nosuch.e.a)": _invalid("e", "e", "23"),
    "insert into public.e values (e.a)": _invalid("e", "e", "30"),
    "insert into e values (1, e.*)": _invalid("e", "e", "26"),
    "insert into e as x values (x.a)": _invalid("x", "x", "28"),
    "insert into e as x values (e.a)": _invalid("e", "x", "28"),
    # A qualifier naming nothing in the range table, a CTE included.
    "insert into e values (x.a)": (
        "42P01",
        'missing FROM-clause entry for table "x"',
        None,
        "23",
    ),
    "insert into e values (s.a)": (
        "42P01",
        'missing FROM-clause entry for table "s"',
        None,
        "23",
    ),
    "with w as (select 1 as q) insert into e values (w.q, 'x')": (
        "42P01",
        'missing FROM-clause entry for table "w"',
        None,
        "49",
    ),
    "with w as (select 1 as q) insert into e values (q, 'x')": (
        "42703",
        'column "q" does not exist',
        None,
        "49",
    ),
    "insert into e values (a.b.c.d)": (
        "0A000",
        "cross-database references are not implemented: a.b.c.d",
        None,
        "23",
    ),
    # Analysis comes before evaluation, and goes row by row: a row's
    # references, then its length, then its assignment types.
    "insert into e values (1/0, bb)": ("42703", 'column "bb" does not exist', _BB_HINT, "28"),
    "insert into e values (true, bb)": ("42703", 'column "bb" does not exist', _BB_HINT, "29"),
    "insert into e values (bb, 'y'), (true, 'x')": (
        "42703",
        'column "bb" does not exist',
        _BB_HINT,
        "23",
    ),
    "insert into e values (true, 'x'), (bb, 'y')": (
        "42804",
        'column "a" is of type integer but expression is of type boolean',
        "You will need to rewrite or cast the expression.",
        "23",
    ),
    "insert into e values (1, 'x'), (bb, 'y', 3)": (
        "42703",
        'column "bb" does not exist',
        _BB_HINT,
        "33",
    ),
    "insert into e values (1, 'x'), (2, 'y', 3), (zz, 'q')": (
        "42601",
        "VALUES lists must all be the same length",
        None,
        "33",
    ),
    "insert into e values (1, 'x', 3), (zz, 'q')": (
        "42601",
        "INSERT has more expressions than target columns",
        None,
        "31",
    ),
    # A generated column's value is refused by the rewriter, after every
    # row is analysed.
    "insert into g values (1, 2), (true, default)": (
        "42804",
        'column "a" is of type integer but expression is of type boolean',
        "You will need to rewrite or cast the expression.",
        "31",
    ),
    "insert into g values (1, 2), (zz, default)": (
        "42703",
        'column "zz" does not exist',
        None,
        "31",
    ),
    # A subquery's own FROM still binds its columns.
    "insert into e values ((select a from s limit 1), 'x')": None,
}


@pytest.mark.backlog_fixed("RP-415")
@pytest.mark.parametrize("query", list(INSERT_VALUES_FORMS))
def test_insert_values_reference_forms(query: str, dsn: str) -> None:
    """The other forms of a column reference in an INSERT's VALUES, each
    refused as a gap here before (or, behind an earlier item, answered with
    that item's run-time error), as PostgreSQL 15.19 answers them."""
    assert answer(dsn, query) == INSERT_VALUES_FORMS[query]


_NO_OPERATOR = (
    "42883",
    "operator does not exist: text = integer",
    "No operator matches the given name and argument types."
    " You might need to add explicit type casts.",
)

JOIN_ON = {
    # An ON sees only its own join's sides, so `b` is `k.b` alone.
    "select * from e, s join k on b = k.a": (*_NO_OPERATOR, "32"),
    "select * from e, (s join k on b = k.a) j": (*_NO_OPERATOR, "33"),
    # The same ON with no comma item before it, refused already.
    "select * from s join k on b = k.a": (*_NO_OPERATOR, "29"),
    # A well-typed ON after a comma item still runs.
    "select * from e, s join k on c = k.b": None,
}


@pytest.mark.backlog_fixed("RP-416")
@pytest.mark.parametrize("query", list(JOIN_ON))
def test_join_on_after_comma_item_is_type_checked(query: str, dsn: str) -> None:
    """A join's ON after a comma item was judged in the whole FROM's
    scope, where `b` named both `e.b` and `k.b`, so its `text = integer`
    went unchecked and the query answered rows (or failed at run time
    with a 0A000 once there were any)."""
    assert answer(dsn, query) == JOIN_ON[query]


SUBQUERY_ON = [
    "select * from e where exists (select 1 from s join k on s.a = k.a and k.b = e.b)",
    "select * from e where exists (select 1 from s join k on k.b = e.b)",
    "select * from e where e.a in (select s.a from s left join k on k.b = e.b)",
]


@pytest.mark.backlog_fixed("RP-430")
@pytest.mark.parametrize("query", SUBQUERY_ON)
def test_subquery_join_on_sees_the_outer_query(query: str, dsn: str) -> None:
    """A subquery's join ON may name the enclosing query's columns, as its
    WHERE may; PostgreSQL 15.19 runs all three. The ON was not searched for
    an outer qualifier, so the subquery planned uncorrelated: the first
    was 42P01 `missing FROM-clause entry for table "e"` and the other two
    42703 `column "b" does not exist`."""
    assert answer(dsn, query) is None


_ROWS_SETUP = [
    "drop table if exists oe, os, ok, ox",
    "create table oe (a int, b text)",
    "create table os (a int, c text)",
    "create table ok (a int, b text, bc int)",
    "create table ox (a int, d text)",
    "insert into oe values (1, 'p'), (2, 'q'), (3, null), (4, 'r'), (null, 'p')",
    "insert into os values (1, 'u'), (2, 'v'), (3, 'w'), (5, 'z')",
    "insert into ok values (1, 'p', 10), (2, 'p', 20), (2, 'q', 30), (3, null, 40), (5, 'r', 50)",
    "insert into ox values (1, 'p'), (2, 'q'), (5, 'r')",
]

_OUTER_PQR = [(1, "p"), (2, "q"), (4, "r"), (None, "p")]

# PostgreSQL 15.19's rows, measured on a private cluster with _ROWS_SETUP.
SUBQUERY_ON_ROWS = {
    "select * from oe where exists (select 1 from os join ok on os.a = ok.a"
    " and ok.b = oe.b) order by 1, 2": _OUTER_PQR,
    "select * from oe where oe.a in (select os.a from os left join ok"
    " on ok.b = oe.b) order by 1, 2": [(1, "p"), (2, "q"), (3, None)],
    "select oe.*, (select count(*) from os join ok on os.a = ok.a"
    " and ok.b = oe.b) from oe order by 1, 2": [
        (1, "p", 2),
        (2, "q", 1),
        (3, None, 0),
        (4, "r", 1),
        (None, "p", 2),
    ],
    "select oe.*, (select count(*) from os right join ok on os.a = ok.a"
    " and ok.b = oe.b) from oe order by 1, 2": [
        (1, "p", 5),
        (2, "q", 5),
        (3, None, 5),
        (4, "r", 5),
        (None, "p", 5),
    ],
    "select oe.*, (select count(*) from os full join ok on os.a = ok.a"
    " and ok.b = oe.b) from oe order by 1, 2": [
        (1, "p", 7),
        (2, "q", 8),
        (3, None, 9),
        (4, "r", 8),
        (None, "p", 7),
    ],
    "select * from oe where exists (select 1 from os join (ok join ox"
    " on ox.a = ok.a and ox.d = oe.b) on os.a = ok.a) order by 1, 2": _OUTER_PQR,
    "select * from oe o where exists (select 1 from os join ok on os.a = ok.a"
    " and ok.b = o.b) order by 1, 2": _OUTER_PQR,
}


@pytest.fixture(scope="module")
def rows_dsn(dsn: str) -> Iterator[str]:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _ROWS_SETUP:
            c.execute(q)
    yield dsn
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(_ROWS_SETUP[0])


@pytest.mark.backlog_fixed("RP-430")
@pytest.mark.parametrize("query", list(SUBQUERY_ON_ROWS))
def test_subquery_join_on_outer_reference_rows(query: str, rows_dsn: str) -> None:
    """The outer reference in a subquery's join ON is read per outer row,
    at any depth of the join tree and under every join type, and a NULL
    outer value matches nothing."""
    with psycopg.connect(rows_dsn, autocommit=True) as c:
        assert c.execute(query).fetchall() == SUBQUERY_ON_ROWS[query]


SUBQUERY_QUALIFIER = {
    "select (select x.a from s limit 1)": (
        "42P01",
        'missing FROM-clause entry for table "x"',
        None,
        "16",
    ),
    # Here this one answers rows.
    "select 1 from k where exists (select x.a from s)": (
        "42P01",
        'missing FROM-clause entry for table "x"',
        None,
        "38",
    ),
    "insert into e values ((select x.a from s limit 1), 'x')": (
        "42P01",
        'missing FROM-clause entry for table "x"',
        None,
        "31",
    ),
    "insert into e values ((select e.a from s limit 1), 'x')": (
        "42P01",
        'invalid reference to FROM-clause entry for table "e"',
        'There is an entry for table "e", but it cannot be referenced from this part of the query.',
        "31",
    ),
}


@pytest.mark.backlog("RP-443")
@pytest.mark.parametrize("query", list(SUBQUERY_QUALIFIER))
def test_subquery_qualifier_naming_nothing(query: str, dsn: str) -> None:
    assert answer(dsn, query) == SUBQUERY_QUALIFIER[query]
