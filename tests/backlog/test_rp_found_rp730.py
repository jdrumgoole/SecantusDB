"""What working RP-730 (an ARRAY sublink refuses ragged or NULL element
arrays) found:

- an element array whose lower bounds are not 1 was lost when an ARRAY
  sublink or `array_agg` accumulated it (RP-731, fixed);
- a query with ORDER BY computed its select list after the sort, so a
  later row's error was raised first (RP-732, fixed).

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _answer(dsn: str, query: str) -> object:
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return (e.sqlstate, str(e).splitlines()[0])


@pytest.mark.backlog_fixed("RP-731")
def test_accumulated_arrays_keep_their_lower_bounds(dsn: str) -> None:
    """`accumArrayResultArr` (PostgreSQL 15.19) copies the first element
    array's lower bounds into the result's inner dimensions. Here each
    bounded element became a NULL, and as text an array of strings, until
    the sublinks and `array_agg` accumulated as it does."""
    assert _answer(
        dsn, "select array(select f from (values ('[2:3]={3,4}'::int[])) t(f))::text"
    ) == [("[1:1][2:3]={{3,4}}",)]
    assert _answer(
        dsn,
        "select array_agg(f)::text"
        " from (values ('[2:3]={3,4}'::int[]), ('[2:3]={5,6}'::int[])) t(f)",
    ) == [("[1:2][2:3]={{3,4},{5,6}}",)]
    # The correlated sublink and the window aggregate accumulate the same way.
    assert _answer(
        dsn,
        "select k, array(select f from (values ('[0:1]={a,b}'::text[])) t(f)"
        " where k = 1)::text from (values (1), (2)) s(k)",
    ) == [(1, "[1:1][0:1]={{a,b}}"), (2, "{}")]
    assert _answer(
        dsn,
        "select array_agg(f) over (order by f)::text"
        " from (values ('[2:3]={3,4}'::int[]), ('[2:3]={5,6}'::int[])) t(f)",
    ) == [("[1:1][2:3]={{3,4}}",), ("[1:2][2:3]={{3,4},{5,6}}",)]


@pytest.mark.backlog_fixed("RP-732")
def test_select_list_is_computed_before_the_sort(dsn: str) -> None:
    """PostgreSQL 15.19 projects the select list below the Sort, a row at a
    time in scan order (`make_sort_input_target`), so the first row
    scanned raises its error, whatever sorts first. Here the select list
    was computed after the sort, as the rows were sent: the row sorting
    first raised its error, and a row a LIMIT dropped never raised one.

    The divisions are by `k - 1`, not by a constant: PostgreSQL folds a
    constant `1/0` while planning and raises it whatever the rows are."""
    assert _answer(
        dsn,
        "select k, case when k = 1 then 1/(k - 1) else ('x' || k)::int end"
        " from (values (1), (2)) s(k) order by k desc",
    ) == ("22012", "division by zero")
    assert _answer(
        dsn, "select k, ('x' || k)::int from (values (1), (2)) s(k) order by k desc"
    ) == ("22P02", 'invalid input syntax for type integer: "x1"')
    # Every row is computed for the Sort, the ones a LIMIT drops too.
    assert _answer(
        dsn,
        "select k, 1/(k - 1) from (values (1), (2)) s(k) order by k desc limit 1",
    ) == ("22012", "division by zero")
    assert _answer(
        dsn,
        "select k, ('x' || k)::int from (values (1), (2)) s(k) order by ('y' || k)::int",
    ) == ("22P02", 'invalid input syntax for type integer: "x1"')
    # A volatile column is postponed past the Sort: computed in output
    # order, and only for the rows the LIMIT keeps.
    assert _answer(
        dsn,
        "select k, 1/(k - 1 + (random() * 0)::int) from (values (1), (2)) s(k)"
        " order by k desc limit 1",
    ) == [(2, 1)]
    # Over a generated series too.
    assert _answer(
        dsn, "select g, ('x' || g)::int from generate_series(1, 2) g order by g desc"
    ) == ("22P02", 'invalid input syntax for type integer: "x1"')
