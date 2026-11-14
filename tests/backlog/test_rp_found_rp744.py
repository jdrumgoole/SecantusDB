"""Gap tests of divergences found while working RP-744 (a DECLARE CURSOR
computed every row's select list at the DECLARE):

- a user function call, a `nextval` or a scalar subquery in a cursor's
  select list was still computed for every row at the DECLARE (fixed,
  RP-895);
- a row a cursor reads again (a backward FETCH, a WITH HOLD cursor
  persisted at COMMIT) is not computed again, where PostgreSQL recomputes
  a volatile column;
- `select * from cursor_to_xml(...)` reads the cursor twice;
- a cursor over a set operation computed its rows at the DECLARE (fixed
  for a UNION ALL whose branches have its types, RP-896), and still runs
  one whose branches unification converts whole;
- a cursor's WHERE and Sort ran at the DECLARE, where PostgreSQL runs
  them at the first FETCH (fixed, RP-897), and its WHERE is still
  evaluated for every row at that FETCH (RP-961);
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
from decimal import Decimal

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


@pytest.mark.backlog_fixed("RP-895")
def test_a_cursor_computes_a_function_call_as_it_is_fetched(dsn: str) -> None:
    """PostgreSQL 15.19 computes a user function call, a `nextval` and a
    scalar subquery in a cursor's select list for the rows a FETCH reaches:
    `fetch 1` answers the first row, and draws the sequence once. Here they
    were computed for every row at the DECLARE, which raised the second
    row's error and drew the sequence three times."""
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


@pytest.mark.backlog_fixed("RP-895")
def test_a_cursors_function_reads_the_snapshot_its_volatility_gives(dsn: str) -> None:
    """A function in a cursor's select list runs at the FETCH that reaches
    its row. PostgreSQL 15.19 runs a STABLE one in the portal's snapshot,
    the DECLARE's, so a row the block inserts after the DECLARE is not
    counted; a VOLATILE one takes a snapshot per statement and counts it.
    A cursor closed unread draws no sequence value. Measured on a private
    PostgreSQL 15.19 cluster."""
    n = uuid.uuid4().hex[:8]
    t, s = f"rp895_t_{n}", f"rp895_s_{n}"
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(f"create table {t} (k int)")
        c.execute(f"create sequence {s}")
        for vol in ("volatile", "stable"):
            c.execute(
                f"create function rp895_{vol}_{n}(int) returns bigint language sql {vol}"
                f" as 'select count(*) from {t}'"
            )

    def counted(vol: str) -> list[object]:
        return _steps(
            dsn,
            [
                "begin",
                f"declare c cursor for select rp895_{vol}_{n}(k) from (values (1), (2)) s(k)",
                "fetch 1 from c",
                f"insert into {t} values (1)",
                "fetch 1 from c",
                "rollback",
            ],
        )

    assert [
        counted("volatile"),
        counted("stable"),
        _steps(
            dsn,
            [
                "begin",
                f"declare c cursor for select nextval('{s}') from generate_series(1, 3)",
                "close c",
                f"select nextval('{s}')",
                "rollback",
            ],
        ),
    ] == [
        ["BEGIN", "DECLARE CURSOR", [(0,)], "INSERT 0 1", [(1,)], "ROLLBACK"],
        ["BEGIN", "DECLARE CURSOR", [(0,)], "INSERT 0 1", [(0,)], "ROLLBACK"],
        ["BEGIN", "DECLARE CURSOR", "CLOSE CURSOR", [(1,)], "ROLLBACK"],
    ]


@pytest.mark.backlog("RP-962")
def test_a_cursor_recomputes_a_volatile_column_when_it_reads_a_row_again(dsn: str) -> None:
    """PostgreSQL 15.19 reads a row again by running the plan again: a
    backward FETCH, or a WITH HOLD cursor persisted at COMMIT
    (`PersistHoldablePortal` rewinds and runs the query whole), computes
    a `nextval` in the select list again. Here the rows computed once are
    kept and read back. Measured on a private PostgreSQL 15.19 cluster."""
    n = uuid.uuid4().hex[:8]
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(f"create sequence rp_reread_a_{n}")
        c.execute(f"create sequence rp_reread_b_{n}")
    assert [
        _steps(
            dsn,
            [
                "begin",
                f"declare c scroll cursor for select nextval('rp_reread_a_{n}')"
                " from generate_series(1, 4)",
                "fetch 2 from c",
                "fetch backward 1 from c",
                "fetch first from c",
                "fetch 2 from c",
                "rollback",
            ],
        ),
        _steps(
            dsn,
            [
                "begin",
                f"declare c cursor with hold for select nextval('rp_reread_b_{n}')"
                " from generate_series(1, 3)",
                "fetch 1 from c",
                "commit",
                f"select currval('rp_reread_b_{n}')",
                "fetch all from c",
                "close c",
            ],
        ),
    ] == [
        ["BEGIN", "DECLARE CURSOR", [(1,), (2,)], [(3,)], [(4,)], [(5,), (6,)], "ROLLBACK"],
        ["BEGIN", "DECLARE CURSOR", [(1,)], "COMMIT", [(4,)], [(3,), (4,)], "CLOSE CURSOR"],
    ]


@pytest.mark.backlog("RP-963")
def test_cursor_to_xml_in_from_reads_the_cursor_once(dsn: str) -> None:
    """`select * from cursor_to_xml('c', 1, ...)` reads one row of the
    cursor on PostgreSQL 15.19, as the call in a select list does. Here the
    call in FROM reads the cursor twice: it answers the second row, and the
    FETCH after it the third. Measured on the shared reference."""
    assert _steps(
        dsn,
        [
            "begin",
            "declare c cursor for select g from generate_series(1, 4) g",
            "select * from cursor_to_xml('c', 1, false, true, '')",
            "fetch 1 from c",
            "rollback",
        ],
    ) == [
        "BEGIN",
        "DECLARE CURSOR",
        [('<row xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">\n  <g>1</g>\n</row>\n\n',)],
        [(2,)],
        "ROLLBACK",
    ]


@pytest.mark.backlog_fixed("RP-896")
def test_a_cursor_over_a_union_computes_its_rows_as_they_are_fetched(dsn: str) -> None:
    """PostgreSQL 15.19 runs a cursor's Append as it is fetched: `fetch 1`
    computes the first branch's first row only. Here the set operation's
    rows were all computed at the DECLARE, which raised the second row's
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


@pytest.mark.backlog_fixed("RP-896")
def test_a_union_all_cursor_reads_its_branches_in_order(dsn: str) -> None:
    """A cursor over a UNION ALL reads its branches as PostgreSQL 15.19's
    Append does: a SCROLL cursor moves back across them; a branch another
    statement of the block made run keeps its error for the FETCH that
    reaches it; and a branch reads the DECLARE's snapshot, so a row the
    block inserts after the DECLARE is in neither. Measured on a private
    PostgreSQL 15.19 cluster."""
    n = uuid.uuid4().hex[:8]
    t = f"rp896_{n}"
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(f"create table {t} (k int)")
        c.execute(f"insert into {t} values (1)")
    assert [
        _steps(
            dsn,
            [
                "begin",
                "declare c scroll cursor for select k from (values (1), (2)) s(k)"
                " union all select k * 10 from (values (3), (4)) s(k) union all select 7",
                "fetch 3 from c",
                "fetch backward 2 from c",
                "move all in c",
                "fetch prior from c",
                "fetch absolute 2 from c",
                "rollback",
            ],
        ),
        _steps(
            dsn,
            [
                "begin",
                "declare c cursor for select k from (values (1), (2)) s(k)"
                " union all select 1/(k - 1) from (values (2), (1)) s(k)",
                "select 1",
                "fetch 3 from c",
                "fetch 1 from c",
                "rollback",
            ],
        ),
        _steps(
            dsn,
            [
                "begin",
                f"declare c cursor for select k from {t} union all select k + 100 from {t}",
                "fetch 1 from c",
                f"insert into {t} values (2)",
                "fetch all from c",
                "rollback",
            ],
        ),
    ] == [
        [
            "BEGIN",
            "DECLARE CURSOR",
            [(1,), (2,), (30,)],
            [(2,), (1,)],
            "MOVE 4",
            [(7,)],
            [(2,)],
            "ROLLBACK",
        ],
        [
            "BEGIN",
            "DECLARE CURSOR",
            [(1,)],
            [(1,), (2,), (1,)],
            ("22012", "division by zero"),
            "ROLLBACK",
        ],
        ["BEGIN", "DECLARE CURSOR", [(1,)], "INSERT 0 1", [(101,)], "ROLLBACK"],
    ]


@pytest.mark.backlog("RP-896")
def test_a_cursor_over_a_converting_union_computes_its_rows_as_they_are_fetched(
    dsn: str,
) -> None:
    """PostgreSQL 15.19's Append converts a branch's rows to the set
    operation's types as it reads them, so `fetch 1` over `select 1/(k - 1)
    ... union all select 2.5` answers the first branch's first row as a
    numeric. Here a branch whose columns unification converts makes the
    whole set operation run at the first FETCH, which raises the second
    row's error. Measured on the shared reference."""
    assert _steps(
        dsn,
        [
            "begin",
            "declare c cursor for select 1/(k - 1) from (values (2), (1)) s(k)"
            " union all select 2.5",
            "fetch 1 from c",
            "rollback",
        ],
    ) == ["BEGIN", "DECLARE CURSOR", [(Decimal("1"),)], "ROLLBACK"]


@pytest.mark.backlog_fixed("RP-897")
def test_a_cursor_runs_its_query_at_the_first_fetch(dsn: str) -> None:
    """PostgreSQL 15.19's DECLARE only plans (`PortalStart`): a WHERE or a
    Sort that raises does so at the first FETCH. Here the query ran at the
    DECLARE, which raised, and the cursor was never made."""
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


@pytest.mark.backlog_fixed("RP-897")
def test_a_deferred_cursor_reads_the_declares_snapshot(dsn: str) -> None:
    """A cursor's query runs at its first FETCH, but it reads what the
    DECLARE's snapshot holds: rows the block inserts or updates after the
    DECLARE are not the cursor's. Measured on a private PostgreSQL 15.19
    cluster."""
    n = uuid.uuid4().hex[:8]
    t = f"rp897_{n}"
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(f"create table {t} (k int)")
        c.execute(f"insert into {t} values (1), (2)")
    assert [
        _steps(
            dsn,
            [
                "begin",
                f"declare c cursor for select k from {t} where k > 0 order by k",
                f"insert into {t} values (3)",
                "fetch all from c",
                f"select count(*) from {t}",
                "rollback",
            ],
        ),
        _steps(
            dsn,
            [
                "begin",
                f"declare c cursor for select k from {t} where k > 0",
                f"update {t} set k = k + 10",
                "fetch all from c",
                "rollback",
            ],
        ),
    ] == [
        ["BEGIN", "DECLARE CURSOR", "INSERT 0 1", [(1,), (2,)], [(3,)], "ROLLBACK"],
        ["BEGIN", "DECLARE CURSOR", "UPDATE 2", [(1,), (2,)], "ROLLBACK"],
    ]


@pytest.mark.backlog_fixed("RP-897")
def test_a_deferred_cursor_raises_at_its_fetch(dsn: str) -> None:
    """A cursor's run that another statement of the block started (to keep
    the DECLARE's snapshot) raises its error at the FETCH that reaches it,
    as PostgreSQL 15.19's would; one no FETCH reads raises nothing, and a
    WITH HOLD one runs whole at the COMMIT, which its error fails.
    Measured on a private PostgreSQL 15.19 cluster."""
    where = "select k from (values (2), (0)) s(k) where 1/k > 0"
    assert [
        _steps(
            dsn,
            [
                "begin",
                f"declare c cursor for {where}",
                "select 1",
                "fetch 1 from c",
                "rollback",
            ],
        ),
        _steps(dsn, ["begin", f"declare c cursor for {where}", "close c", "select 1", "commit"]),
        _steps(
            dsn,
            ["begin", f"declare c cursor with hold for {where}", "commit", "fetch 1 from c"],
        ),
        _steps(
            dsn,
            [
                "begin",
                "declare c cursor with hold for select k, 1/(k - 1) from (values (2), (3)) s(k)"
                " order by k",
                "commit",
                "fetch all from c",
                "close c",
            ],
        ),
        _steps(
            dsn,
            [
                "begin",
                f"declare c cursor for {where}",
                "select name, statement from pg_cursors",
                "fetch 1 from c",
                "rollback",
            ],
        ),
    ] == [
        ["BEGIN", "DECLARE CURSOR", [(1,)], ("22012", "division by zero"), "ROLLBACK"],
        ["BEGIN", "DECLARE CURSOR", "CLOSE CURSOR", [(1,)], "COMMIT"],
        [
            "BEGIN",
            "DECLARE CURSOR",
            ("22012", "division by zero"),
            ("34000", 'cursor "c" does not exist'),
        ],
        ["BEGIN", "DECLARE CURSOR", "COMMIT", [(2, 1), (3, 0)], "CLOSE CURSOR"],
        [
            "BEGIN",
            "DECLARE CURSOR",
            [("c", f"declare c cursor for {where}")],
            ("22012", "division by zero"),
            "ROLLBACK",
        ],
    ]


@pytest.mark.backlog("RP-961")
def test_a_cursors_where_filters_as_it_is_fetched(dsn: str) -> None:
    """PostgreSQL 15.19 filters a cursor's rows as it is fetched: with no
    Sort, `fetch 1` evaluates the WHERE only up to the first row that
    passes, so a later row's error is raised by the FETCH that reaches it.
    Here the first FETCH evaluates the WHERE for every row and raises.
    Measured on a private PostgreSQL 15.19 cluster."""
    assert _steps(
        dsn,
        [
            "begin",
            "declare c cursor for select k from (values (1), (2), (0)) s(k) where 10/k > 1",
            "fetch 1 from c",
            "fetch 1 from c",
            "fetch 1 from c",
            "rollback",
        ],
    ) == [
        "BEGIN",
        "DECLARE CURSOR",
        [(1,)],
        [(2,)],
        ("22012", "division by zero"),
        "ROLLBACK",
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
