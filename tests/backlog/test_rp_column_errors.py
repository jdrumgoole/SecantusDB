"""Gap tests of the missing-column and ON-clause errors found while working
RP-408: RP-413 (a function in FROM leaves a 42703 with no HINT), RP-414 (a
42703 names the wrong reference or drops its qualifier), RP-415 (a column
in an INSERT's VALUES is refused as a gap) and RP-416 (a join's ON after a
comma item was not type-checked; fixed), and RP-430, found while fixing
RP-416 (a subquery's join ON could not name the outer query; fixed).

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
    "drop table if exists e, s, k",
    "create table e (a int, b text)",
    "create table s (a int, c text)",
    "create table k (a int, b text, bc int)",
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


@pytest.mark.backlog("RP-415")
@pytest.mark.parametrize("query", list(INSERT_VALUES))
def test_insert_values_column_reference(query: str, dsn: str) -> None:
    assert answer(dsn, query) == INSERT_VALUES[query]


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
