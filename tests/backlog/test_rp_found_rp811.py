"""Gap tests of the divergences found while working RP-811 (a table's
CHECK constraint coerced to boolean):

- a domain's CHECK and a partial index's WHERE are not coerced to boolean
  (RP-813);
- a boolean coercion passes an array, and names a domain by its base type
  (RP-814);
- ALTER TABLE's expressions are not coerced to boolean when no row reaches
  them, and SET DEFAULT's error has a position (RP-815);
- a system column in a CHECK was 42703, not 42P10 (RP-816).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists c811, t811 cascade",
    "drop domain if exists di811, d811 cascade",
    "create domain di811 as int",
    "create table c811 (x int, a int[], d di811)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])
        c.execute(_SETUP[1])


def _answer(dsn: str, query: str) -> object:
    """The rows or status, or the error's SQLSTATE, first line and
    position."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0], e.diag.statement_position)
        return cur.fetchall() if cur.description else cur.statusmessage


def _not_boolean(test: str, typ: str, position: int | None) -> tuple[str, str, str, str | None]:
    return (
        "error",
        "42804",
        f"argument of {test} must be type boolean, not type {typ}",
        None if position is None else str(position),
    )


@pytest.mark.backlog("RP-813")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("create domain d811 as int check (value)", _not_boolean("CHECK", "integer", None)),
        (
            "create domain d811 as int check (value is true)",
            _not_boolean("IS TRUE", "integer", None),
        ),
        ("create index on c811 (x) where x", _not_boolean("WHERE", "integer", 32)),
    ],
)
def test_a_domain_check_and_an_index_predicate_are_coerced_to_boolean(
    dsn: str, query: str, expected: object
) -> None:
    """`domainAddConstraint` coerces a domain's CHECK to boolean (with no
    position: it has no query text) and `transformIndexStmt` a partial
    index's WHERE: each is 42804 on PostgreSQL 15.19. Here the domain and
    the index are created."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog_fixed("RP-814")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("select * from c811 where a", _not_boolean("WHERE", "integer[]", 26)),
        ("select * from c811 where a and true", _not_boolean("AND", "integer[]", 26)),
        ("select a is true from c811", _not_boolean("IS TRUE", "integer[]", 8)),
        ("create table t811 (a int[] check (a))", _not_boolean("CHECK", "integer[]", 35)),
        ("select * from c811 where d", _not_boolean("WHERE", "di811", 26)),
        ("select * from c811 where d and true", _not_boolean("AND", "di811", 26)),
    ],
)
def test_a_boolean_coercion_refuses_an_array_and_names_a_domain(
    dsn: str, query: str, expected: object
) -> None:
    """`coerce_to_boolean` refuses any type with no cast to boolean -- an
    array included -- and names the operand's own type, a domain by the
    domain's name: each is 42804 on PostgreSQL 15.19. An array passed (the
    SELECTs answered no rows, the table was created) and a domain was named
    by its base type."""
    assert _answer(dsn, query) == expected


_BOOL_SETUP = [
    "drop table if exists p814",
    "drop domain if exists dd814, db814, da814 cascade",
    "create domain db814 as bool",
    "create domain dd814 as db814",
    "create domain da814 as int[]",
    "create table p814 (b bool, db db814, dd dd814, a int[], ba bool[], da da814, x int)",
    "insert into p814 values (true, true, true, '{1}', '{t}', '{2}', 1)",
]


@pytest.mark.backlog_fixed("RP-814")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("select x from p814 where db", [(1,)]),
        ("select x from p814 where dd and true", [(1,)]),
        ("select db is true, dd is false from p814", [(True, False)]),
        ("select x from p814 where ba[1]", [(1,)]),
        ("select x from p814 where not db", []),
        ("select x from p814 p where p.db", [(1,)]),
        ("select x from p814 where da", _not_boolean("WHERE", "da814", 26)),
        ("select x from p814 where ba", _not_boolean("WHERE", "boolean[]", 26)),
        (
            "select x from p814 where a is not unknown",
            _not_boolean("IS NOT UNKNOWN", "integer[]", 26),
        ),
    ],
)
def test_a_boolean_coercion_follows_a_domain_to_its_base(
    dsn: str, query: str, expected: object
) -> None:
    """A domain over `boolean` (or over such a domain) coerces to boolean,
    and one over an array is refused under its own name; `boolean[]` is
    no boolean. Measured on a private PostgreSQL 15.19 cluster."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _BOOL_SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
            got: object = cur.fetchall()
        except psycopg.Error as e:
            got = ("error", e.sqlstate, str(e).splitlines()[0], e.diag.statement_position)
        finally:
            c.execute(_BOOL_SETUP[0])
            c.execute(_BOOL_SETUP[1])
    assert got == expected


@pytest.mark.backlog("RP-815")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (
            "alter table c811 alter column x type bool using (x is true)",
            _not_boolean("IS TRUE", "integer", 50),
        ),
        (
            "alter table c811 add column y bool generated always as (x is true) stored",
            _not_boolean("IS TRUE", "integer", None),
        ),
        (
            "alter table c811 alter column x set default (1 is true)",
            _not_boolean("IS TRUE", "integer", None),
        ),
    ],
)
def test_alter_table_expressions_are_coerced_to_boolean(
    dsn: str, query: str, expected: object
) -> None:
    """ALTER TABLE transforms a USING, a generated column's and a DEFAULT's
    expression whatever the rows, so an `int` under IS TRUE is 42804 on
    PostgreSQL 15.19 over an empty table; the generated column's and the
    DEFAULT's go through `AddRelationNewConstraints`, which ALTER TABLE
    hands no query text, so they carry no position. Here the first two are
    `ALTER TABLE` and the third is placed at the `1`."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog_fixed("RP-816")
def test_a_system_column_in_a_check_is_42p10(dsn: str) -> None:
    """`scanNSItemForColumn` refuses a system column other than `tableoid`
    in a CHECK: 42P10 on PostgreSQL 15.19. It was 42703, as though the
    column did not exist."""
    assert _answer(dsn, "create table t811 (x int check (xmin))") == (
        "error",
        "42P10",
        'system column "xmin" reference in check constraint is invalid',
        "33",
    )
