"""Gap tests of the divergences found while working RP-860 (a whole-row
reference in a CHECK):

- a column renamed under a CHECK leaves the CHECK naming the old column;
- `pg_get_constraintdef` drops `NOT VALID` from a CHECK added NOT VALID;
- a CHECK comparing `regtype` values prints without the `oid` casts.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each script runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t860 cascade",
    "create table t860 (x int)",
]

_DEF = "select pg_get_constraintdef(oid) from pg_constraint where conrelid = 't860'::regclass"


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


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


@pytest.mark.backlog_fixed("RP-939")
def test_a_renamed_column_is_renamed_in_its_check(dsn: str) -> None:
    """A CHECK holds the column itself, not its name: after `rename column
    x to y` it still tests the column, now printed `y`, on PostgreSQL
    15.19. The stored text still named `x`, so every INSERT was 42703
    `column "x" does not exist`."""
    assert _script(
        dsn,
        [
            "alter table t860 add constraint k check (x > 0)",
            "alter table t860 rename column x to y",
            "insert into t860 values (1)",
            "insert into t860 values (0)",
            _DEF,
        ],
    ) == [
        "ALTER TABLE",
        "ALTER TABLE",
        "INSERT 0 1",
        ("error", "23514", 'new row for relation "t860" violates check constraint "k"', None),
        [("CHECK ((y > 0))",)],
    ]


@pytest.mark.backlog_fixed("RP-940")
def test_a_not_valid_check_prints_not_valid(dsn: str) -> None:
    """`pg_get_constraintdef` ends a CHECK that is not validated with ` NOT
    VALID` on PostgreSQL 15.19. It was left off."""
    assert _script(
        dsn,
        ["alter table t860 add constraint k check (x > 0) not valid", _DEF],
    ) == ["ALTER TABLE", [("CHECK ((x > 0)) NOT VALID",)]]


@pytest.mark.backlog_fixed("RP-941")
def test_a_regtype_comparison_in_a_check_prints_its_oid_casts(dsn: str) -> None:
    """`regtype` has no `=` of its own: the parser compares two of them as
    `oid` (`oideq` over binary-coercible casts), and ruleutils prints the
    casts, `((pg_typeof(x))::oid = ('integer'::regtype)::oid)`, on
    PostgreSQL 15.19. It was printed as written, `(pg_typeof(x) =
    'int4'::regtype)`, until RP-941."""
    assert _script(
        dsn,
        ["alter table t860 add constraint k check (pg_typeof(x) = 'int4'::regtype)", _DEF],
    ) == ["ALTER TABLE", [("CHECK (((pg_typeof(x))::oid = ('integer'::regtype)::oid))",)]]


@pytest.mark.backlog_fixed("RP-1024")
def test_a_renamed_partition_key_column_still_routes(dsn: str) -> None:
    """The partition key holds the column's attnum: after `rename column x
    to y` on a partitioned table, a row still routes and the key prints
    `LIST (y)` on PostgreSQL 15.19 (shared reference, port 5415). The
    stored key text still named `x`, so the INSERT was 42703 `column "x"
    does not exist`. Found while working RP-939."""
    assert _script(
        dsn,
        [
            "drop table if exists p939k",
            "create table p939k (x int) partition by list (x)",
            "create table p939k1 partition of p939k for values in (1)",
            "alter table p939k rename column x to y",
            "insert into p939k values (1)",
            "select pg_get_partkeydef('p939k'::regclass)",
            "drop table p939k",
        ],
    ) == [
        "DROP TABLE",
        "CREATE TABLE",
        "CREATE TABLE",
        "ALTER TABLE",
        "INSERT 0 1",
        [("LIST (y)",)],
        "DROP TABLE",
    ]


@pytest.mark.backlog("RP-1052")
def test_pg_get_partkeydef_of_an_expression_key(dsn: str) -> None:
    """`pg_get_partkeydef` prints an expression key as ruleutils does,
    `RANGE (((x + 1)))` on PostgreSQL 15.19 (shared reference, port 5415):
    the key list's parentheses, the expression's own, and the OpExpr's.
    Here one layer is missing. Found while working RP-1024."""
    assert _script(
        dsn,
        [
            "drop table if exists p1024e",
            "create table p1024e (x int) partition by range ((x + 1))",
            "select pg_get_partkeydef('p1024e'::regclass)",
            "drop table p1024e",
        ],
    ) == ["DROP TABLE", "CREATE TABLE", [("RANGE (((x + 1)))",)], "DROP TABLE"]


@pytest.mark.backlog("RP-1053")
def test_a_renamed_column_is_renamed_in_a_generated_column(dsn: str) -> None:
    """A generated column's expression holds the column's attnum: after
    `rename column x to y` an INSERT still computes it, `(3, 6)`, on
    PostgreSQL 15.19 (shared reference, port 5415). Here the stored
    expression still names `x`, so the INSERT is 42703. Found while
    working RP-1024."""
    assert _script(
        dsn,
        [
            "drop table if exists p1024g",
            "create table p1024g (x int, g int generated always as (x * 2) stored)",
            "alter table p1024g rename column x to y",
            "insert into p1024g (y) values (3)",
            "select * from p1024g",
            "drop table p1024g",
        ],
    ) == ["DROP TABLE", "CREATE TABLE", "ALTER TABLE", "INSERT 0 1", [(3, 6)], "DROP TABLE"]


@pytest.mark.backlog("RP-1054")
def test_a_renamed_column_is_renamed_in_an_expression_index(dsn: str) -> None:
    """An index expression and predicate hold the column's attnum: after
    `rename column x to y` an INSERT still maintains the index, which
    prints `(((y + 1))) WHERE (y > 0)`, on PostgreSQL 15.19 (shared
    reference, port 5415). Here both still name `x` and the INSERT is
    42703. Found while working RP-1024."""
    assert _script(
        dsn,
        [
            "drop table if exists p1024i",
            "create table p1024i (x int)",
            "create index p1024ii on p1024i ((x + 1)) where x > 0",
            "alter table p1024i rename column x to y",
            "insert into p1024i values (3)",
            "select pg_get_indexdef('p1024ii'::regclass)",
            "drop table p1024i",
        ],
    ) == [
        "DROP TABLE",
        "CREATE TABLE",
        "CREATE INDEX",
        "ALTER TABLE",
        "INSERT 0 1",
        [("CREATE INDEX p1024ii ON public.p1024i USING btree (((y + 1))) WHERE (y > 0)",)],
        "DROP TABLE",
    ]
