"""Array subscripting and slicing — ``arr[i]`` (1-based element access, NULL when
out of range) and ``arr[lo:hi]`` (1-based inclusive slice), in the SELECT list and
in WHERE. ``unnest(arr_col)`` in the SELECT list is covered by the set-returning
function tests; here we pin the subscript/slice semantics.

Postgres arrays are 1-based; ``arr[0]`` and any out-of-range index yield NULL (no
Python-style negative wraparound). A slice clamps to the array bounds.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB, user="secantus")


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, tags text[], nums int[])")
    run(storage, session, "INSERT INTO t VALUES (1, ARRAY['a','b','c'], ARRAY[10,20,30])")
    run(storage, session, "INSERT INTO t VALUES (2, ARRAY['x','y'], ARRAY[7])")
    return storage


# -- single-element subscript ------------------------------------------------- #


def test_subscript_first_element(t, session):
    assert run(t, session, "SELECT id, tags[1] FROM t ORDER BY id").rows == [(1, "a"), (2, "x")]


def test_subscript_second_element(t, session):
    assert run(t, session, "SELECT tags[2] FROM t ORDER BY id").rows == [("b",), ("y",)]


def test_subscript_int_element(t, session):
    assert run(t, session, "SELECT nums[3] FROM t WHERE id = 1").rows == [(30,)]


def test_subscript_out_of_range_is_null(t, session):
    assert run(t, session, "SELECT tags[9] FROM t WHERE id = 1").rows == [(None,)]


def test_subscript_zero_is_null(t, session):
    # Postgres arrays are 1-based; index 0 is out of range -> NULL (no wraparound).
    assert run(t, session, "SELECT tags[0] FROM t WHERE id = 1").rows == [(None,)]


def test_subscript_element_type_is_element_not_array(t, session):
    cols = run(t, session, "SELECT nums[1] AS x FROM t WHERE id = 1").columns
    assert cols[0].type_tag == "int4"


def test_subscript_runtime_index(t, session):
    # A column-bearing index is the true 1-based value: id=1 -> tags[1], id=2 -> tags[2].
    assert run(t, session, "SELECT tags[id] FROM t ORDER BY id").rows == [("a",), ("y",)]


# -- slice -------------------------------------------------------------------- #


def test_slice_middle(t, session):
    assert run(t, session, "SELECT tags[2:3] FROM t WHERE id = 1").rows == [(["b", "c"],)]


def test_slice_from_start(t, session):
    assert run(t, session, "SELECT tags[1:2] FROM t WHERE id = 1").rows == [(["a", "b"],)]


def test_slice_clamps_upper(t, session):
    assert run(t, session, "SELECT tags[2:99] FROM t WHERE id = 1").rows == [(["b", "c"],)]


def test_slice_type_is_array(t, session):
    cols = run(t, session, "SELECT tags[1:2] AS s FROM t WHERE id = 1").columns
    assert cols[0].type_tag == "text[]"


# -- subscript in WHERE ------------------------------------------------------- #


def test_where_subscript_equality(t, session):
    assert run(t, session, "SELECT id FROM t WHERE tags[1] = 'a'").rows == [(1,)]


def test_where_subscript_range(t, session):
    assert run(t, session, "SELECT id FROM t WHERE nums[1] > 8 ORDER BY id").rows == [(1,)]


def test_where_subscript_no_match(t, session):
    assert run(t, session, "SELECT id FROM t WHERE tags[1] = 'zzz'").rows == []


# --- multidimensional subscripting, and assignment ---------------------------
#
# Measured against PostgreSQL 14.13. Every case below used to leak a raw Python
# error to the wire with NO SQLSTATE at all (`invalid literal for int() with
# base 10: '{1,2}'`), or was refused as `0A000 expected a column, got: ia[1]` --
# a subscript the user never wrote, because sqlglot had folded it to 0-based.


@pytest.fixture
def m(storage, session):
    run(storage, session, "CREATE TABLE m (id int PRIMARY KEY, ia int[], mm int[][], n int)")
    run(
        storage,
        session,
        "INSERT INTO m VALUES (1, ARRAY[1,2,3], ARRAY[[1,2],[3,4]], 2), (2, NULL, NULL, 1)",
    )
    return storage


def test_a_short_subscript_list_selects_nothing(m, session):
    """`(ARRAY[[1,2],[3,4]])[1]` is NULL on PostgreSQL, not the inner row.

    Returning the inner row handed a list to a column declared `int4`, and the
    `int('{1,2}')` that followed reached the client as a bare Python ValueError
    with no SQLSTATE.
    """
    assert run(m, session, "SELECT mm[1], mm[1][2] FROM m WHERE id=1").rows == [(None, 2)]


def test_a_bare_index_beside_a_slice_means_one_to_n(m, session):
    """PostgreSQL: once ANY subscript is a slice, a subscript written as a
    single number is "from 1 to the number specified".

    So `mm[1:2][2]` is the WHOLE second dimension, not its second element --
    and `mm[1:2][1]` agrees with the `n:n` reading, which is exactly why a
    probe that only tried `[1]` would call the wrong rule correct.
    """
    row = run(m, session, "SELECT mm[1:2][1], mm[1:2][2], mm[2:2] FROM m WHERE id=1").rows[0]
    assert row == ([[1], [3]], [[1, 2], [3, 4]], [[3, 4]])


def test_a_subscript_bound_may_read_the_row(m, session):
    assert run(m, session, "SELECT ia[n], ia[n:3], ia[1:n] FROM m WHERE id=1").rows == [
        (2, [2, 3], [1, 2])
    ]


def test_assigning_into_an_array_extends_it_with_nulls(m, session):
    """`SET a[i] = v` rewrites the stored array rather than replacing it, and a
    subscript past the end pads the gap with NULLs."""
    assert run(m, session, "UPDATE m SET ia[2] = 99 WHERE id=1 RETURNING ia").rows == [
        ([1, 99, 3],)
    ]
    assert run(m, session, "UPDATE m SET ia[6] = 6 WHERE id=1 RETURNING ia").rows == [
        ([1, 99, 3, None, None, 6],)
    ]
    # Two assignments to one column in one statement: the second sees the
    # first, rather than both starting from the pre-image.
    assert run(m, session, "UPDATE m SET ia[1] = 7, ia[2] = 8 WHERE id=1 RETURNING ia").rows == [
        ([7, 8, 3, None, None, 6],)
    ]
    # Assigning into a NULL column builds the array from nothing.
    assert run(m, session, "UPDATE m SET ia[1] = 1 WHERE id=2 RETURNING ia").rows == [([1],)]
    assert run(m, session, "UPDATE m SET mm[1][2] = 42 WHERE id=1 RETURNING mm").rows == [
        ([[1, 42], [3, 4]],)
    ]
    # The rewrite is STORED, not only returned.
    assert run(m, session, "SELECT ia, mm FROM m WHERE id=1").rows == [
        ([7, 8, 3, None, None, 6], [[1, 42], [3, 4]])
    ]


def test_slice_assignment_needs_a_source_that_fills_the_range(m, session):
    assert run(m, session, "UPDATE m SET ia[2:3] = ARRAY[4,5] WHERE id=1 RETURNING ia").rows == [
        ([1, 4, 5],)
    ]
    # A source LONGER than the range has its tail ignored.
    assert run(m, session, "UPDATE m SET ia[1:2] = ARRAY[8,9,10] WHERE id=1 RETURNING ia").rows == [
        ([8, 9, 5],)
    ]
    with pytest.raises(Exception) as info:
        run(m, session, "UPDATE m SET ia[1:2] = ARRAY[1] WHERE id=1")
    assert "source array too small" in str(info.value)


def test_assigning_below_subscript_1_is_refused(m, session):
    """PostgreSQL answers it by MOVING the array's lower bound -- `SET ia[0]=0`
    leaves an `[0:5]={...}`. This server does not model lower bounds, so
    writing it at 1 instead would silently shift every other subscript."""
    with pytest.raises(Exception) as info:
        run(m, session, "UPDATE m SET ia[0] = 0 WHERE id=1")
    assert getattr(info.value, "sqlstate", None) == "0A000"
    assert run(m, session, "SELECT ia FROM m WHERE id=1").rows == [([1, 2, 3],)]


def test_a_size_postgres_refuses_is_refused_before_it_is_allocated(m, session):
    """`SET a[1000000000] = 1` is one line and would otherwise allocate every
    slot it names. PostgreSQL caps an array at 134217727 elements and says so."""
    with pytest.raises(Exception) as info:
        run(m, session, "UPDATE m SET ia[1000000000] = 1 WHERE id=1")
    assert "134217727" in str(info.value)
    assert run(m, session, "SELECT ia FROM m WHERE id=1").rows == [([1, 2, 3],)]
