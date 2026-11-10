"""Gap tests of the divergences found while working RP-808 (`IS TRUE` and
its kin over a value that is not boolean):

- an UPDATE's SET expression is not coerced to boolean when no row reaches
  it (RP-810);
- a CHECK constraint's expression was not coerced to boolean (RP-811,
  fixed);
- AND / OR / NOT report a non-boolean argument at the operator, not at the
  argument's leftmost token (RP-812).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t808, c808 cascade",
    "create table t808 (i int, b bool, s text)",
    "insert into t808 values (1, true, 'x'), (null, null, null)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


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


def _not_boolean(test: str, typ: str, position: int) -> tuple[str, str, str, str]:
    return (
        "error",
        "42804",
        f"argument of {test} must be type boolean, not type {typ}",
        str(position),
    )


@pytest.mark.backlog_fixed("RP-808")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("select i is true from t808", _not_boolean("IS TRUE", "integer", 8)),
        ("select * from t808 order by i is true", _not_boolean("IS TRUE", "integer", 29)),
        ("select * from t808 a join t808 b on a.i is true", _not_boolean("IS TRUE", "integer", 37)),
        (
            "select count(*) from t808 group by i having i is not unknown",
            _not_boolean("IS NOT UNKNOWN", "integer", 45),
        ),
        ("update t808 set b = (i is not false)", _not_boolean("IS NOT FALSE", "integer", 22)),
        ("delete from t808 where s is false", _not_boolean("IS FALSE", "text", 24)),
        ("select coalesce(i, 0) is true from t808", _not_boolean("IS TRUE", "integer", 8)),
        ("select (s || 'y') is true from t808", _not_boolean("IS TRUE", "text", 9)),
        ("select ('x' || 'y') is true", _not_boolean("IS TRUE", "text", 9)),
        ("select (select i) is false from t808", _not_boolean("IS FALSE", "integer", 8)),
        (
            "select b is true, b is not true, b is false, b is not false, b is unknown,"
            " b is not unknown from t808 order by b",
            [(True, False, False, True, False, True), (False, True, False, True, True, False)],
        ),
        ("select 'yes' is true, 'off' is not false, null is unknown", [(True, False, True)]),
    ],
)
def test_a_boolean_test_over_a_non_boolean_is_42804_at_its_argument(
    dsn: str, query: str, expected: object
) -> None:
    """`transformBooleanTest` coerced its argument to boolean naming the
    test, reported at the argument's `exprLocation` -- an operator
    expression's leftmost token, a COALESCE's keyword. Before RP-808 an
    UPDATE's `i is not false` over an `int` column wrote `coalesce(i,
    true)` into the rows, and `('x' || 'y') is true` answered `xy`."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog("RP-810")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("update t808 set b = (i is true) where false", _not_boolean("IS TRUE", "integer", 22)),
        ("update t808 set b = (i and true) where false", _not_boolean("AND", "integer", 22)),
        ("update t808 set b = (not i) where false", _not_boolean("NOT", "integer", 26)),
    ],
)
def test_an_update_set_expression_is_coerced_to_boolean(
    dsn: str, query: str, expected: object
) -> None:
    """`transformUpdateTargetList` transforms each SET expression, which
    coerces a test's or a connective's argument to boolean whether or not a
    row is updated: each is 42804 on PostgreSQL 15.19. Here an UPDATE no
    row reaches is `UPDATE 0`."""
    assert _answer(dsn, query) == expected


def _unpositioned(error: tuple[str, str, str, str]) -> tuple[str, str, str, None]:
    return (error[0], error[1], error[2], None)


@pytest.mark.backlog_fixed("RP-811")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("create table c808 (x int check (x))", _not_boolean("CHECK", "integer", 33)),
        ("create table c808 (x int check (x is true))", _not_boolean("IS TRUE", "integer", 33)),
        ("create table c808 (x int check (x and true))", _not_boolean("AND", "integer", 33)),
        ("create table c808 (x int, check (x + 1))", _not_boolean("CHECK", "integer", 34)),
        ("create table c808 (x text check (x || 'a'))", _not_boolean("CHECK", "text", 34)),
        ("create table c808 (x int check (x::text))", _not_boolean("CHECK", "text", 33)),
        (
            "create table c808 (x int check (case when x > 0 then 1 else 0 end))",
            _not_boolean("CHECK", "integer", 33),
        ),
        ("create table c808 (x int check (coalesce(x, 1)))", _not_boolean("CHECK", "integer", 33)),
        ("create table c808 (x int check (x > 0 and x))", _not_boolean("AND", "integer", 43)),
        ("create table c808 (x int check (not x))", _not_boolean("NOT", "integer", 37)),
        ("create table c808 (x int check (1))", _not_boolean("CHECK", "integer", 33)),
        # `transformExpr` goes left to right, and the whole is coerced last.
        ("create table c808 (x int check (x and y))", _not_boolean("AND", "integer", 33)),
        (
            "create table c808 (x int check ((x is true) = y))",
            _not_boolean("IS TRUE", "integer", 34),
        ),
        (
            "create table c808 (x int check (y and x))",
            ("error", "42703", 'column "y" does not exist', "33"),
        ),
        (
            "create table c808 (x int check (x + y))",
            ("error", "42703", 'column "y" does not exist', "37"),
        ),
        (
            "create table c808 (x int check ('x'))",
            ("error", "22P02", 'invalid input syntax for type boolean: "x"', "33"),
        ),
        ("create table c808 (x int check ('t'))", "CREATE TABLE"),
        ("create table c808 (x int check (null))", "CREATE TABLE"),
        ("create table c808 (x int check (x::bool))", "CREATE TABLE"),
        ("create table c808 (x int check (x in (1, 2)))", "CREATE TABLE"),
        ("create table c808 (x bool check (x))", "CREATE TABLE"),
        # ALTER TABLE hands `AddRelationNewConstraints` no query text.
        ("alter table t808 add check (i)", _unpositioned(_not_boolean("CHECK", "integer", 0))),
        (
            "alter table t808 add constraint k check (i is false)",
            _unpositioned(_not_boolean("IS FALSE", "integer", 0)),
        ),
        (
            "alter table t808 add column y int check (y + 1)",
            _unpositioned(_not_boolean("CHECK", "integer", 0)),
        ),
        (
            "alter table t808 add constraint k check (q)",
            ("error", "42703", 'column "q" does not exist', None),
        ),
        (
            "alter table t808 add constraint k check ('x')",
            ("error", "22P02", 'invalid input syntax for type boolean: "x"', None),
        ),
    ],
)
def test_a_check_constraint_is_coerced_to_boolean(dsn: str, query: str, expected: object) -> None:
    """`cookConstraint` coerces a CHECK expression to boolean
    (`coerce_to_boolean(..., "CHECK")`) after transforming it: each is 42804
    on PostgreSQL 15.19, with no position under ALTER TABLE. Before RP-811
    the table was created, and `check (x)` then admitted every row, 0 and
    NULL included, while `check (x is true)` refused every one."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog("RP-812")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("select ('x' || 'y') and true", _not_boolean("AND", "text", 9)),
        ("select not ('x' || 'y')", _not_boolean("NOT", "text", 13)),
        ("select (s || 'y') or true from t808", _not_boolean("OR", "text", 9)),
        ("insert into t808 (b) values (1 and true)", _not_boolean("AND", "integer", 30)),
    ],
)
def test_a_connectives_argument_is_reported_at_its_leftmost_token(
    dsn: str, query: str, expected: object
) -> None:
    """`coerce_to_boolean` reports at `exprLocation` of the argument, which
    for an operator expression is its leftmost token: `('x' || 'y') and
    true` is 42804 at the `'x'` on PostgreSQL 15.19. Here it is reported at
    the `||` (or the `and`)."""
    assert _answer(dsn, query) == expected
