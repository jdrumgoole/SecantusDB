"""Gap tests of the divergences found while working RP-816 (a system column
in a CHECK refused with 42P10):

- `tableoid` in a CHECK names no column (fixed, RP-859);
- a CHECK naming its own table by `regclass` is refused at CREATE;
- a partition's CHECK violation names the partitioned table;
- a whole-row reference in a CHECK is refused as unsupported (fixed,
  RP-860);
- a CHECK's qualified column reference: another table's name is accepted,
  and a missing column is named without its qualifier;
- a system column in a generated column's expression is accepted.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t816, c816 cascade",
    "create table c816 (x int)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str) -> object:
    """The status, or the error's SQLSTATE, first line and position."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0], e.diag.statement_position)
        return cur.statusmessage


@pytest.mark.backlog_fixed("RP-859")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("create table t816 (x int check (tableoid::int > 0))", "CREATE TABLE"),
        ("alter table c816 add constraint k check (tableoid::int > 0)", "ALTER TABLE"),
        (
            "create table t816 (x int check (tableoid))",
            ("error", "42804", "argument of CHECK must be type boolean, not type oid", "33"),
        ),
    ],
)
def test_tableoid_in_a_check_is_a_column(dsn: str, query: str, expected: object) -> None:
    """`scanNSItemForColumn` lets a CHECK name `tableoid`, the one system
    column it allows: the constraint is created on PostgreSQL 15.19, and a
    bare `tableoid` is coerced to boolean like any `oid`. Here each was
    42703 `column "tableoid" does not exist`."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog_fixed("RP-859")
def test_a_checks_tableoid_is_the_rows_relation(dsn: str) -> None:
    """A CHECK's `tableoid` is the oid of the relation the row is stored
    in: its own table, or the leaf partition a row of a partitioned table
    lands in (PostgreSQL 15.19 answers below), and the generated name is
    `<table>_tableoid_check`."""
    got = _script(
        dsn,
        [
            "drop table if exists p859",
            "alter table c816 add check (tableoid = 'c816'::regclass)",
            "insert into c816 values (1)",
            "create table p859 (x int, check (tableoid::regclass::text = 'p859a'))"
            " partition by list (x)",
            "create table p859a partition of p859 for values in (1)",
            "create table p859b partition of p859 for values in (2)",
            "insert into p859 values (1)",
            "insert into p859 values (2)",
            "drop table p859",
        ],
    )
    assert got == [
        "DROP TABLE",
        "ALTER TABLE",
        "INSERT 0 1",
        "CREATE TABLE",
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 1",
        (
            "error",
            "23514",
            'new row for relation "p859b" violates check constraint "p859_tableoid_check"',
            None,
        ),
        "DROP TABLE",
    ]


@pytest.mark.backlog_fixed("RP-860")
def test_a_whole_row_reference_in_a_check_is_taken(dsn: str) -> None:
    """A CHECK may test the whole row: `check ((t816.*) is not null)`
    creates the table on PostgreSQL 15.19. Here it was 0A000 `this column
    is not supported yet`."""
    assert _answer(dsn, "create table t816 (x int check ((t816.*) is not null))") == (
        "CREATE TABLE"
    )


def _script(dsn: str, statements: list[str]) -> list[object]:
    """Each statement's status, rows or error, after `_SETUP`."""
    out: list[object] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        for q in statements:
            try:
                cur = c.execute(q)
            except psycopg.Error as e:
                out.append(("error", e.sqlstate, str(e).splitlines()[0], e.diag.statement_position))
                continue
            out.append(cur.fetchall() if cur.description else cur.statusmessage)
    return out


_DEF = "select pg_get_constraintdef(oid) from pg_constraint where conrelid = 't816'::regclass"
_VIOLATES = (
    "error",
    "23514",
    'new row for relation "t816" violates check constraint "t816_check"',
    None,
)


@pytest.mark.backlog_fixed("RP-860")
@pytest.mark.parametrize(
    ("statements", "expected"),
    [
        (
            [
                "create table t816 (x int, y text, check (t816 is not null))",
                "insert into t816 values (1, 'a')",
                "insert into t816 values (1, null)",
                _DEF,
            ],
            ["CREATE TABLE", "INSERT 0 1", _VIOLATES, [("CHECK ((t816.* IS NOT NULL))",)]],
        ),
        (
            [
                "create table t816 (x int, y int, check (row(t816.*) is not null))",
                "insert into t816 values (1, null)",
                _DEF,
            ],
            ["CREATE TABLE", _VIOLATES, [("CHECK ((ROW(x, y) IS NOT NULL))",)]],
        ),
        (
            [
                "create table t816 (x int, y text, check (t816::text <> '(1,a)'))",
                "insert into t816 values (1, 'a')",
                "insert into t816 values (2, 'a')",
                _DEF,
            ],
            [
                "CREATE TABLE",
                _VIOLATES,
                "INSERT 0 1",
                [("CHECK (((t816.*)::text <> '(1,a)'::text))",)],
            ],
        ),
        (
            [
                "create table t816 (x int, check ((t816).x > 0))",
                "insert into t816 values (0)",
                _DEF,
            ],
            [
                "CREATE TABLE",
                (
                    "error",
                    "23514",
                    'new row for relation "t816" violates check constraint "t816_x_check"',
                    None,
                ),
                [("CHECK ((x > 0))",)],
            ],
        ),
        (
            [
                "create table t816 (x int, check ((t816.*) = row(1)::t816))",
                "insert into t816 values (2)",
                "insert into t816 values (1)",
            ],
            ["CREATE TABLE", _VIOLATES, "INSERT 0 1"],
        ),
        (
            # The row a CHECK tests is the table's row as it is now.
            [
                "create table t816 (x int check ((t816.*) is not null))",
                "alter table t816 add column z int",
                "insert into t816 values (1, null)",
                "insert into t816 values (1, 2)",
            ],
            [
                "CREATE TABLE",
                "ALTER TABLE",
                _VIOLATES,
                "INSERT 0 1",
            ],
        ),
        (
            [
                "create table t816 (x int, check ((t816.*).nosuch > 0))",
                "create table t816 (x int, check (c816.* is not null))",
                "create table t816 (x int, check (t816.*))",
                "select 1 from c816 where c816",
            ],
            [
                ("error", "42703", "column t816.nosuch does not exist", "35"),
                ("error", "42P01", 'missing FROM-clause entry for table "c816"', "34"),
                ("error", "42804", "argument of CHECK must be type boolean, not type t816", "34"),
                ("error", "42804", "argument of WHERE must be type boolean, not type c816", "26"),
            ],
        ),
        (
            [
                "insert into c816 values (null)",
                "alter table c816 add constraint k check ((c816.*) is not null)",
                "delete from c816",
                "alter table c816 add constraint k check ((c816.*) is not null)",
                "insert into c816 values (null)",
            ],
            [
                "INSERT 0 1",
                (
                    "error",
                    "23514",
                    'check constraint "k" of relation "c816" is violated by some row',
                    None,
                ),
                "DELETE 1",
                "ALTER TABLE",
                (
                    "error",
                    "23514",
                    'new row for relation "c816" violates check constraint "k"',
                    None,
                ),
            ],
        ),
    ],
)
def test_a_whole_row_check_is_enforced(
    dsn: str, statements: list[str], expected: list[object]
) -> None:
    """A CHECK over the whole row is analysed as `transformExpr` analyses
    it over the constraint's one range-table entry, enforced per row, and
    printed as `pg_get_constraintdef` prints the analysed tree. Each answer
    measured on a private PostgreSQL 15.19 cluster."""
    assert _script(dsn, statements) == expected


@pytest.mark.backlog_fixed("RP-861")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (
            "create table t816 (x int, check (other.x = 1))",
            ("error", "42P01", 'missing FROM-clause entry for table "other"', "34"),
        ),
        (
            "create table t816 (x int, check (t816.nosuch = 1))",
            ("error", "42703", "column t816.nosuch does not exist", "34"),
        ),
    ],
)
def test_a_checks_qualified_column_is_resolved_against_its_table(
    dsn: str, query: str, expected: object
) -> None:
    """A CHECK's range table holds only its own table, so another table's
    name is 42P01 at the reference, and a missing column written with its
    qualifier is named with it (`errorMissingColumn`) at the reference's
    start, on PostgreSQL 15.19. The first table was created here and the
    second error named `"nosuch"` at the column."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog_fixed("RP-862")
def test_a_system_column_in_a_generated_column_is_42p10(dsn: str) -> None:
    """`scanNSItemForColumn` refuses a system column other than `tableoid`
    in a generation expression: 42P10 on PostgreSQL 15.19. The table was
    created here."""
    assert _answer(
        dsn, "create table t816 (x int, y int generated always as (xmin::text::int) stored)"
    ) == (
        "error",
        "42P10",
        'cannot use system column "xmin" in column generation expression',
        "54",
    )


@pytest.mark.backlog_fixed("RP-974")
def test_a_checks_regclass_of_its_own_table_resolves_at_create(dsn: str) -> None:
    """`cookConstraint` runs after `heap_create_with_catalog`, so a CHECK
    may name its own table by `regclass`: `create table t816 (x int, check
    (tableoid = 't816'::regclass))` is CREATE TABLE on PostgreSQL 15.19.
    It was 42P01 `relation "t816" does not exist` at 14."""
    assert _answer(dsn, "create table t816 (x int, check (tableoid = 't816'::regclass))") == (
        "CREATE TABLE"
    )


@pytest.mark.backlog_fixed("RP-975")
def test_a_partitions_check_violation_names_the_leaf(dsn: str) -> None:
    """`ExecConstraints` reports the leaf partition's result relation: a
    row routed through `p974` into `p974b` that fails a CHECK is 23514
    `new row for relation "p974b" ...` on PostgreSQL 15.19. It named the
    partitioned table, `p974`."""
    got = _script(
        dsn,
        [
            "drop table if exists p974",
            "create table p974 (x int check (x < 2)) partition by list (x)",
            "create table p974b partition of p974 for values in (2)",
            "insert into p974 values (2)",
            "drop table p974",
        ],
    )
    assert got[3] == (
        "error",
        "23514",
        'new row for relation "p974b" violates check constraint "p974_x_check"',
        None,
    )
