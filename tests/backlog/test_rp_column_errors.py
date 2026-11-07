"""Gap tests of the missing-column and ON-clause errors found while working
RP-408: RP-413 (a function in FROM leaves a 42703 with no HINT), RP-414 (a
42703 names the wrong reference or drops its qualifier), RP-415 (a column
in an INSERT's VALUES is refused as a gap) and RP-416 (a join's ON after a
comma item is not type-checked).

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
    "select * from e, s join k on b = k.a": (*_NO_OPERATOR, "32"),
    # The same ON with no comma item before it, refused already.
    "select * from s join k on b = k.a": (*_NO_OPERATOR, "29"),
}


@pytest.mark.backlog("RP-416")
@pytest.mark.parametrize("query", list(JOIN_ON))
def test_join_on_after_comma_item_is_type_checked(query: str, dsn: str) -> None:
    assert answer(dsn, query) == JOIN_ON[query]
