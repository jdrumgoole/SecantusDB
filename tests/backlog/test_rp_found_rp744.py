"""Gap tests of divergences found while working RP-744 (a DECLARE CURSOR
computed every row's select list at the DECLARE):

- a user function call, a `nextval` or a scalar subquery in a cursor's
  select list is still computed for every row at the DECLARE;
- a cursor over a set operation computes its rows at the DECLARE;
- a cursor's WHERE and Sort run at the DECLARE, where PostgreSQL runs
  them at the first FETCH;
- `FETCH FORWARD 0` / `FETCH 0` / `FETCH BACKWARD 0` answered no row, and
  `MOVE FORWARD 0` moved over none (fixed, RP-898);
- `pg_cursors.statement` is a deparse of the query, not the DECLARE text;
- a PL/pgSQL `FETCH` from a `refcursor` parameter is refused at CREATE
  FUNCTION.

The expected answers were measured on PostgreSQL 15.19: the VALUES-only
statements on the shared reference, the ones that create a function or a
sequence on a private cluster.
"""

from __future__ import annotations

import uuid
from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _steps(dsn: str, steps: list[str]) -> list[object]:
    """Each statement's rows, command tag, or (SQLSTATE, first line)."""
    out: list[object] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for s in steps:
            try:
                cur = c.execute(s)
                out.append(cur.fetchall() if cur.description else cur.statusmessage)
            except psycopg.Error as e:
                out.append((e.sqlstate, str(e).splitlines()[0]))
    return out


@pytest.mark.backlog("RP-895")
def test_a_cursor_computes_a_function_call_as_it_is_fetched(dsn: str) -> None:
    """PostgreSQL 15.19 computes a user function call, a `nextval` and a
    scalar subquery in a cursor's select list for the rows a FETCH reaches:
    `fetch 1` answers the first row, and draws the sequence once. Here they
    are computed for every row at the DECLARE, which raises the second
    row's error and draws the sequence three times."""
    n = uuid.uuid4().hex[:8]
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(
            f"create function rp744_f_{n}(int) returns int language sql as 'select 1/($1 - 1)'"
        )
        c.execute(f"create sequence rp744_s_{n}")
    assert [
        _steps(
            dsn,
            [
                "begin",
                f"declare c cursor for select rp744_f_{n}(k) from (values (2), (1)) s(k)",
                "fetch 1 from c",
                "rollback",
            ],
        ),
        _steps(
            dsn,
            [
                "begin",
                f"declare c cursor for select nextval('rp744_s_{n}') from generate_series(1, 3)",
                "fetch 1 from c",
                f"select currval('rp744_s_{n}')",
                "rollback",
            ],
        ),
        _steps(
            dsn,
            [
                "begin",
                "declare c cursor for select (select 1/(k - 1)) from (values (2), (1)) s(k)",
                "fetch 1 from c",
                "rollback",
            ],
        ),
    ] == [
        ["BEGIN", "DECLARE CURSOR", [(1,)], "ROLLBACK"],
        ["BEGIN", "DECLARE CURSOR", [(1,)], [(1,)], "ROLLBACK"],
        ["BEGIN", "DECLARE CURSOR", [(1,)], "ROLLBACK"],
    ]


@pytest.mark.backlog("RP-896")
def test_a_cursor_over_a_union_computes_its_rows_as_they_are_fetched(dsn: str) -> None:
    """PostgreSQL 15.19 runs a cursor's Append as it is fetched: `fetch 1`
    computes the first branch's first row only. Here the set operation's
    rows are all computed at the DECLARE, which raises the second row's
    error."""
    assert _steps(
        dsn,
        [
            "begin",
            "declare c cursor for select 1/(k - 1) from (values (2), (1)) s(k) union all select 5",
            "fetch 1 from c",
            "rollback",
        ],
    ) == ["BEGIN", "DECLARE CURSOR", [(1,)], "ROLLBACK"]


@pytest.mark.backlog("RP-897")
def test_a_cursor_runs_its_query_at_the_first_fetch(dsn: str) -> None:
    """PostgreSQL 15.19's DECLARE only plans (`PortalStart`): a WHERE or a
    Sort that raises does so at the first FETCH. Here the query runs at the
    DECLARE, which raises, and the cursor is never made."""
    assert [
        _steps(
            dsn,
            [
                "begin",
                "declare c cursor for select k from (values (2), (0)) s(k) where 1/k > 0",
                "fetch 1 from c",
                "rollback",
            ],
        ),
        _steps(
            dsn,
            [
                "begin",
                "declare c cursor for select k, 1/(k - 1) from (values (2), (1)) s(k)"
                " order by k desc",
                "fetch 1 from c",
                "rollback",
            ],
        ),
    ] == [
        ["BEGIN", "DECLARE CURSOR", ("22012", "division by zero"), "ROLLBACK"],
        ["BEGIN", "DECLARE CURSOR", ("22012", "division by zero"), "ROLLBACK"],
    ]


@pytest.mark.backlog_fixed("RP-898")
def test_a_zero_count_fetch_refetches_the_current_row(dsn: str) -> None:
    """PostgreSQL 15.19 answers `FETCH FORWARD 0`, `FETCH 0` and `FETCH
    BACKWARD 0` with the row the cursor sits on, and `MOVE FORWARD 0`
    reports `MOVE 1`. Here the three answered no row and the MOVE reported
    `MOVE 0`."""
    assert _steps(
        dsn,
        [
            "begin",
            "declare c cursor for select g from generate_series(1, 3) g",
            "fetch 2 from c",
            "fetch forward 0 from c",
            "fetch 0 from c",
            "fetch backward 0 from c",
            "fetch relative 0 from c",
            "move forward 0 in c",
            "rollback",
        ],
    ) == [
        "BEGIN",
        "DECLARE CURSOR",
        [(1,), (2,)],
        [(2,)],
        [(2,)],
        [(2,)],
        [(2,)],
        "MOVE 1",
        "ROLLBACK",
    ]


@pytest.mark.backlog_fixed("RP-898")
def test_a_zero_count_fetch_steps_back_on_a_no_scroll_cursor(dsn: str) -> None:
    """PostgreSQL 15.19 re-fetches the current row by stepping back one row
    and forward one (`DoPortalRunFetch`), so on a NO SCROLL cursor sitting
    on a row a zero-count FETCH -- `RELATIVE 0` included -- is 55000, where
    one before the first row answers no row and a MOVE only counts the
    row. Measured on the shared reference."""
    declare = "declare c no scroll cursor for select g from generate_series(1, 3) g"
    assert _steps(
        dsn,
        [
            "begin",
            declare,
            "fetch 0 from c",
            "move 0 in c",
            "fetch 1 from c",
            "move forward 0 in c",
            "move backward 0 in c",
            "fetch backward 0 from c",
            "rollback",
            "begin",
            declare,
            "fetch 1 from c",
            "fetch relative 0 from c",
            "rollback",
        ],
    ) == [
        "BEGIN",
        "DECLARE CURSOR",
        [],
        "MOVE 0",
        [(1,)],
        "MOVE 1",
        "MOVE 1",
        ("55000", "cursor can only scan forward"),
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        [(1,)],
        ("55000", "cursor can only scan forward"),
        "ROLLBACK",
    ]


@pytest.mark.backlog_fixed("RP-898")
def test_a_plpgsql_zero_count_step_sees_the_current_row(dsn: str) -> None:
    """In PL/pgSQL on PostgreSQL 15.19, `FETCH RELATIVE 0 ... INTO` reads
    the row the cursor sits on and `MOVE RELATIVE 0` / `MOVE FORWARD 0`
    set FOUND by it; on a NO SCROLL cursor the FETCH is 55000 and the MOVE
    is not. Here a MOVE 0 set FOUND false and the NO SCROLL FETCH read the
    row. DO blocks, measured on the shared reference."""
    notices: list[str] = []
    blocks = [
        "do $$ declare c scroll cursor for select g from generate_series(1, 3) g;"
        " r int; begin open c; fetch c into r; fetch c into r;"
        " fetch relative 0 from c into r; raise notice 'r=% found=%', r, found;"
        " move relative 0 in c; raise notice 'm=%', found;"
        " move forward 0 in c; raise notice 'm0=%', found; end $$",
        "do $$ declare c scroll cursor for select g from generate_series(1, 3) g;"
        " r int; begin open c; move forward 0 in c; raise notice 'm=%', found;"
        " fetch relative 0 from c into r; raise notice 'r=% found=%', r, found;"
        " end $$",
        "do $$ declare c no scroll cursor for select g from generate_series(1, 3) g;"
        " r int; begin open c; fetch c into r; move forward 0 in c;"
        " raise notice 'm=%', found; end $$",
        "do $$ declare c no scroll cursor for select g from generate_series(1, 3) g;"
        " r int; begin open c; fetch c into r; fetch relative 0 from c into r;"
        " end $$",
    ]
    out: list[object] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        for b in blocks:
            try:
                c.execute(b)
                out.append("ok")
            except psycopg.Error as e:
                out.append((e.sqlstate, str(e).splitlines()[0]))
    assert (out, notices) == (
        ["ok", "ok", "ok", ("55000", "cursor can only scan forward")],
        ["r=2 found=t", "m=t", "m0=t", "m=f", "r=<NULL> found=f", "m=t"],
    )


@pytest.mark.backlog_fixed("RP-898")
def test_cursor_to_xml_of_zero_rows_refetches_the_current_row(dsn: str) -> None:
    """`cursor_to_xml(c, 0, ...)` reads through `SPI_cursor_fetch`, the
    same `DoPortalRunFetch` step as `FETCH 0`: on PostgreSQL 15.19 it
    answers the row the cursor sits on and leaves it there, nothing before
    the first row or past the last, and 55000 on a NO SCROLL cursor sitting
    on a row. Here it answered no row. Measured on the shared reference."""
    xml0 = "select cursor_to_xml('c', 0, false, true, '')"
    row1 = '<row xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">\n  <g>1</g>\n</row>\n\n'
    assert _steps(
        dsn,
        [
            "begin",
            "declare c cursor for select g from generate_series(1, 3) g",
            xml0,
            "fetch 1 from c",
            xml0,
            "fetch next from c",
            "fetch all from c",
            xml0,
            "rollback",
            "begin",
            "declare c no scroll cursor for select g from generate_series(1, 3) g",
            xml0,
            "fetch 1 from c",
            xml0,
            "rollback",
        ],
    ) == [
        "BEGIN",
        "DECLARE CURSOR",
        [("",)],
        [(1,)],
        [(row1,)],
        [(2,)],
        [(3,)],
        [("",)],
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        [("",)],
        [(1,)],
        ("55000", "cursor can only scan forward"),
        "ROLLBACK",
    ]


@pytest.mark.backlog_fixed("RP-899")
def test_a_duplicate_cursor_name_aborts_the_block(dsn: str) -> None:
    """PostgreSQL 15.19 refuses a DECLARE of a name already open with
    42P03, and the block is then aborted (25P02). This server let the block
    run on."""
    assert _steps(
        dsn,
        [
            "begin",
            "declare c cursor for select 1",
            "declare c cursor for select 2",
            "select 1",
            "rollback",
        ],
    ) == [
        "BEGIN",
        "DECLARE CURSOR",
        ("42P03", 'cursor "c" already exists'),
        (
            "25P02",
            "current transaction is aborted, commands ignored until end of transaction block",
        ),
        "ROLLBACK",
    ]


@pytest.mark.backlog_fixed("RP-900")
def test_pg_cursors_statement_is_the_declare_text(dsn: str) -> None:
    """PostgreSQL 15.19's `pg_cursors.statement` is the query string the
    DECLARE was submitted as, verbatim. This server reported a deparse of
    the inner query."""
    assert _steps(
        dsn,
        [
            "begin",
            "declare  c   cursor for select 1 -- x",
            "select statement from pg_cursors",
            "rollback",
        ],
    ) == ["BEGIN", "DECLARE CURSOR", [("declare  c   cursor for select 1 -- x",)], "ROLLBACK"]


@pytest.mark.backlog("RP-901")
def test_a_refcursor_parameter_can_be_fetched(dsn: str) -> None:
    """PostgreSQL 15.19 creates a PL/pgSQL function that FETCHes from its
    `refcursor` parameter, and the call reads the caller's cursor. Here the
    CREATE FUNCTION is refused, 42601 `variable "c" must be of type cursor
    or refcursor`."""
    n = uuid.uuid4().hex[:8]
    assert _steps(
        dsn,
        [
            f"create function rp744_fetch_{n}(c refcursor) returns int language plpgsql as "
            "$$ declare r int; begin fetch c into r; return r; end $$",
            "begin",
            "declare c cursor for select k from (values (7), (8)) s(k)",
            f"select rp744_fetch_{n}('c')",
            "rollback",
        ],
    ) == ["CREATE FUNCTION", "BEGIN", "DECLARE CURSOR", [(7,)], "ROLLBACK"]
