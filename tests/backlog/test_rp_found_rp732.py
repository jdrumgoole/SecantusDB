"""What working RP-732 (a query with ORDER BY computes its select list
below the Sort) found:

- a DECLARE CURSOR computed every row's select list at the DECLARE, so a
  row no FETCH reaches raised its error there (RP-744, fixed);
- a sorted cursor computed an expensive column below its Sort, where
  PostgreSQL's `cursor_tuple_fraction` postpones it (RP-745, fixed);
- a cast over a CASE of untyped literals reports a branch's input error
  at the literal's position.

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
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


def _fetch_one(dsn: str, declare: str) -> object:
    """The first row a cursor declared by `declare` fetches, or the
    error's SQLSTATE and first line."""
    with psycopg.connect(dsn) as c:
        try:
            c.execute(declare)
            return c.execute("fetch 1 from c").fetchall()
        except psycopg.Error as e:
            return (e.sqlstate, str(e).splitlines()[0])


@pytest.mark.backlog_fixed("RP-744")
def test_a_cursor_computes_its_rows_as_they_are_fetched(dsn: str) -> None:
    """PostgreSQL 15.19 runs a cursor's plan as it is fetched: `fetch 1`
    computes the first row only, so the second row's division by zero is
    never raised. Here DECLARE computed every row and raised it; the
    cursor now keeps the row stream and computes what a FETCH reaches."""
    assert _fetch_one(dsn, "declare c cursor for select 1/(k - 1) from (values (2), (1)) s(k)") == [
        (1,)
    ]


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


# A cursor computing its rows as they are fetched (RP-744), through every
# way a cursor is read. Each answer measured on PostgreSQL 15.19.
CURSOR_READS = {
    "scroll back over computed rows": (
        [
            "begin",
            "declare c scroll cursor for select 1/(k - 1) from (values (2), (3), (1)) s(k)",
            "fetch 2 from c",
            "fetch backward 1 from c",
            "fetch absolute 1 from c",
            "fetch relative 1 from c",
            "fetch 1 from c",
            "rollback",
        ],
        [
            "BEGIN",
            "DECLARE CURSOR",
            [(1,), (0,)],
            [(1,)],
            [(1,)],
            [(0,)],
            ("22012", "division by zero"),
            "ROLLBACK",
        ],
    ),
    "fetch last computes every row": (
        [
            "begin",
            "declare c scroll cursor for select 1/(k - 1) from (values (2), (1)) s(k)",
            "fetch last from c",
            "rollback",
        ],
        ["BEGIN", "DECLARE CURSOR", ("22012", "division by zero"), "ROLLBACK"],
    ),
    "move counts the rows it computes": (
        [
            "begin",
            "declare c cursor for select 1/(k - 1) from (values (2), (1)) s(k)",
            "move 1 from c",
            "fetch next from c",
            "rollback",
        ],
        ["BEGIN", "DECLARE CURSOR", "MOVE 1", ("22012", "division by zero"), "ROLLBACK"],
    ),
    "another statement between fetches computes nothing": (
        [
            "begin",
            "declare c cursor for select 1/(k - 1) from (values (2), (1)) s(k)",
            "fetch 1 from c",
            "select 7",
            "fetch 1 from c",
            "rollback",
        ],
        [
            "BEGIN",
            "DECLARE CURSOR",
            [(1,)],
            [(7,)],
            ("22012", "division by zero"),
            "ROLLBACK",
        ],
    ),
    "a with hold cursor computes the rest at commit": (
        [
            "begin",
            "declare c cursor with hold for select 1/(k - 1) from (values (2), (1)) s(k)",
            "fetch 1 from c",
            "commit",
            "fetch 1 from c",
        ],
        [
            "BEGIN",
            "DECLARE CURSOR",
            [(1,)],
            ("22012", "division by zero"),
            ("34000", 'cursor "c" does not exist'),
        ],
    ),
    "a with hold cursor is read after commit": (
        [
            "begin",
            "declare c cursor with hold for select k * 2 from (values (2), (1), (3)) s(k)",
            "fetch 1 from c",
            "commit",
            "fetch all from c",
            "close c",
        ],
        ["BEGIN", "DECLARE CURSOR", [(4,)], "COMMIT", [(2,), (6,)], "CLOSE CURSOR"],
    ),
    "typed columns read as fetched": (
        [
            "begin",
            "declare c cursor for select k::text || 'x', k::numeric / 3, k > 1"
            " from (values (2), (1), (3)) s(k)",
            "fetch 2 from c",
            "fetch 5 from c",
            "rollback",
        ],
        [
            "BEGIN",
            "DECLARE CURSOR",
            [
                ("2x", Decimal("0.66666666666666666667"), True),
                ("1x", Decimal("0.33333333333333333333"), False),
            ],
            [("3x", Decimal("1.00000000000000000000"), True)],
            "ROLLBACK",
        ],
    ),
}


@pytest.mark.backlog_fixed("RP-744")
@pytest.mark.parametrize("case", CURSOR_READS)
def test_a_cursor_is_read_as_it_computes_its_rows(dsn: str, case: str) -> None:
    """A cursor's rows are computed as FETCH, MOVE, a scroll back and
    COMMIT (a `WITH HOLD` cursor's rest, as `PreCommit_Portals` does) reach
    them, and a statement between two FETCHes computes none. Each answer
    as PostgreSQL 15.19 gives it."""
    steps, expected = CURSOR_READS[case]
    assert _steps(dsn, steps) == expected


@pytest.mark.backlog_fixed("RP-744")
def test_a_binary_fetch_reads_the_rows_as_they_are_computed(dsn: str) -> None:
    """psycopg's binary server cursor asks for BINARY on each FETCH: the
    values a FETCH computes are re-encoded, and the row it never reaches
    raises nothing. PostgreSQL 15.19 answers the first two rows, then
    22012 for the third."""
    with psycopg.connect(dsn) as c:
        cur = c.cursor("bc", binary=True)
        cur.execute("select k, k::text, k::numeric / 4, 1/(k - 1) from (values (2), (3), (1)) s(k)")
        first = cur.fetchmany(2)
        with pytest.raises(psycopg.Error) as info:
            cur.fetchmany(2)
    assert (first, info.value.sqlstate) == (
        [
            (2, "2", Decimal("0.50000000000000000000"), 1),
            (3, "3", Decimal("0.75000000000000000000"), 0),
        ],
        "22012",
    )


@pytest.mark.backlog_fixed("RP-744")
def test_a_plpgsql_fetch_computes_one_row(dsn: str) -> None:
    """A PL/pgSQL FETCH from a cursor the caller declared computes the row
    it reads, and the next FETCH the next (PostgreSQL 15.19: `1`, then
    22012)."""
    n = uuid.uuid4().hex[:8]
    assert _steps(
        dsn,
        [
            f"create function rp744_fetch_{n}(n text) returns int language plpgsql as "
            "$$ declare r int; cc refcursor := n; begin fetch cc into r; return r; end $$",
            "begin",
            "declare c cursor for select 1/(k - 1) from (values (2), (1)) s(k)",
            f"select rp744_fetch_{n}('c')",
            f"select rp744_fetch_{n}('c')",
            "rollback",
        ],
    ) == [
        "CREATE FUNCTION",
        "BEGIN",
        "DECLARE CURSOR",
        [(1,)],
        ("22012", "division by zero"),
        "ROLLBACK",
    ]


@pytest.mark.backlog_fixed("RP-745")
def test_a_sorted_cursor_postpones_an_expensive_column(dsn: str) -> None:
    """A cursor is planned with `cursor_tuple_fraction` (0.1), which
    `make_sort_input_target` treats as a LIMIT: an expensive column
    (`to_tsvector`, `procost` 100) is postponed past the Sort, so `fetch
    1` computes it for the row that sorts first only. PostgreSQL 15.19
    answers that row; here the column was computed below the Sort for
    every row, and the first row's missing configuration was raised."""
    assert _fetch_one(
        dsn,
        "declare c cursor for select k, to_tsvector(case when k = 1 then 'nope'"
        " else 'simple' end::regconfig, 'a')::text from (values (1), (2)) s(k)"
        " order by k desc",
    ) == [(2, "'a':1")]


@pytest.mark.backlog_fixed("RP-745")
def test_cursor_tuple_fraction_decides_the_postponement(dsn: str) -> None:
    """`standard_planner` plans a cursor for the whole result when
    `cursor_tuple_fraction` is 1 or more, so the expensive column is
    computed below the Sort for every row and the first row's error is
    raised; at 0 it takes the smallest fraction above 0 and postpones it.
    Measured on PostgreSQL 15.19."""
    declare = (
        "declare c cursor for select k, to_tsvector(case when k = 1 then 'nope'"
        " else 'simple' end::regconfig, 'a')::text from (values (1), (2)) s(k)"
        " order by k desc"
    )
    answers = []
    for fraction in ("1", "0"):
        with psycopg.connect(dsn) as c:
            c.execute(f"set cursor_tuple_fraction = {fraction}")
            try:
                c.execute(declare)
                answers.append(c.execute("fetch 1 from c").fetchall())
            except psycopg.Error as e:
                answers.append((e.sqlstate, str(e).splitlines()[0]))
    assert answers == [
        ("42704", 'text search configuration "nope" does not exist'),
        [(2, "'a':1")],
    ]


@pytest.mark.backlog("RP-746")
def test_a_cast_over_a_case_of_literals_reports_no_position(dsn: str) -> None:
    """PostgreSQL 15.19 resolves a CASE of untyped literals to `text` and
    casts its result as the row runs, so the input error has no position.
    Here the cast is applied to each branch's literal and the error points
    at it."""
    with psycopg.connect(dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as info:
        c.execute("select (case when k = 1 then 'x' else '5' end)::int from (values (1)) s(k)")
    e = info.value
    assert (e.sqlstate, str(e).splitlines()[0], e.diag.statement_position) == (
        "22P02",
        'invalid input syntax for type integer: "x"',
        None,
    )
