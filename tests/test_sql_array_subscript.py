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
