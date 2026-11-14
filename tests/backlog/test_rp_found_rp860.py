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


@pytest.mark.backlog("RP-939")
def test_a_renamed_column_is_renamed_in_its_check(dsn: str) -> None:
    """A CHECK holds the column itself, not its name: after `rename column
    x to y` it still tests the column, now printed `y`, on PostgreSQL
    15.19. Here the stored text still names `x`, so every INSERT is 42703
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


@pytest.mark.backlog("RP-940")
def test_a_not_valid_check_prints_not_valid(dsn: str) -> None:
    """`pg_get_constraintdef` ends a CHECK that is not validated with ` NOT
    VALID` on PostgreSQL 15.19. Here it is left off."""
    assert _script(
        dsn,
        ["alter table t860 add constraint k check (x > 0) not valid", _DEF],
    ) == ["ALTER TABLE", [("CHECK ((x > 0)) NOT VALID",)]]


@pytest.mark.backlog("RP-941")
def test_a_regtype_comparison_in_a_check_prints_its_oid_casts(dsn: str) -> None:
    """`regtype` has no `=` of its own: the parser compares two of them as
    `oid` (`oideq` over binary-coercible casts), and ruleutils prints the
    casts, `((pg_typeof(x))::oid = ('integer'::regtype)::oid)`, on
    PostgreSQL 15.19. Here it is printed as written, `(pg_typeof(x) =
    'int4'::regtype)`."""
    assert _script(
        dsn,
        ["alter table t860 add constraint k check (pg_typeof(x) = 'int4'::regtype)", _DEF],
    ) == ["ALTER TABLE", [("CHECK (((pg_typeof(x))::oid = ('integer'::regtype)::oid))",)]]
