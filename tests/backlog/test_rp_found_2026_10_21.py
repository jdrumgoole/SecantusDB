"""Gap tests for the Rust PG server items found on 2026-10-21 by the first
weekly sqllogictest run against it (`tasks/backlog-rust-postgres.md`, "Found
by the weekly SQL gauges").

The expected answers are literals measured on a PRIVATE PostgreSQL 15.19
cluster (`initdb --locale=C -E UTF8`), because the scenario creates a table,
which the shared reference must not be given.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


def _rows(conn: Any, sql: str) -> Any:
    """The rows of `sql`, or its SQLSTATE."""
    got = pg_answer(conn, sql)
    return got[1] if got[0] == "ok" else ("err", got[1])


# ------------------------------------------ RP-075 strict NULL folding --


def strict_null_folding(conn: Any) -> tuple:
    conn.execute("create table rp075 (c int, d int)")
    conn.execute("insert into rp075 values (5, 1), (7, 2)")
    return (
        # A strict operator with a NULL constant argument folds to NULL at
        # plan time, so `c / 0` is never evaluated.
        _rows(conn, "select count(*) from rp075 where c / 0 = NULL"),
        _rows(conn, "select count(*) from rp075 where NULL = c / 0"),
        _rows(conn, "select (c / 0 = NULL) is null from rp075 order by d"),
        _rows(conn, "select count(*) from rp075 where d > 0 and c / 0 = NULL"),
        _rows(conn, "select count(*) from rp075 where d > 5 or c / 0 = NULL"),
        _rows(conn, "select count(*) from rp075 where c / 0 + NULL > 1"),
        # Not foldable: IS NULL is not strict, and `d` is not a constant.
        # PostgreSQL evaluates the division and raises, so a fix that
        # swallows the error instead of folding fails here.
        _rows(conn, "select count(*) from rp075 where c / 0 is null"),
        _rows(conn, "select count(*) from rp075 where c / 0 = d"),
    )


# --------------------------------------------- RP-076 NULLIF result type --


def nullif_result_type(conn: Any) -> tuple:
    return (
        _rows(
            conn,
            "select pg_typeof(nullif(53, 80::numeric))::text,"
            " pg_typeof(nullif(53, 80.5))::text,"
            " pg_typeof(nullif(1, 2::float8))::text",
        ),
        # The answers that type changes: integer division here is wrong.
        _rows(conn, "select (nullif(53, 80::numeric) / -71)::text, (nullif(53, 2.5) / 2)::text"),
        # Same-type and cross-type `=` operators keep the first argument's
        # type, so the rule is the operator's input type, not "promote".
        _rows(
            conn,
            "select pg_typeof(nullif(80::numeric, 53))::text,"
            " pg_typeof(nullif(1::int2, 2::int8))::text",
        ),
    )


EXPECTED = {
    "nullif_result_type": (
        [("numeric", "numeric", "double precision")],
        [("-0.74647887323943661972", "26.5000000000000000")],
        [("numeric", "smallint")],
    ),
    "strict_null_folding": (
        [(0,)],
        [(0,)],
        [(True,), (True,)],
        [(0,)],
        [(0,)],
        [(0,)],
        ("err", "22012"),
        ("err", "22012"),
    ),
}


@pytest.mark.backlog("RP-075")
def test_strict_null_folding(pg: Any) -> None:
    """PostgreSQL 15.19 folds `c / 0 = NULL` (and `NULL = ...`, `... + NULL`)
    to NULL before it runs, so the division by zero never happens; the plan
    is `One-Time Filter: false`. `c / 0 IS NULL` and `c / 0 = d` still raise
    22012."""
    assert strict_null_folding(pg) == EXPECTED["strict_null_folding"]


@pytest.mark.backlog("RP-076")
def test_nullif_result_type(pg: Any) -> None:
    """PostgreSQL 15.19 gives NULLIF the left input type of the `=` operator
    it resolves: `nullif(53, 80::numeric)` is numeric (int = numeric resolves
    to numeric = numeric), so dividing it by -71 is -0.7464..., not 0.
    `nullif(int2, int8)` stays smallint, through the cross-type int28eq."""
    assert nullif_result_type(pg) == EXPECTED["nullif_result_type"]
