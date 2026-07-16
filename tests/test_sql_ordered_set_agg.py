"""Ordered-set aggregates: ``percentile_cont`` / ``percentile_disc`` / ``mode``
via ``WITHIN GROUP (ORDER BY expr)``.

They collect the ORDER BY values (via a ``$push`` accumulator) and the executor
sorts + computes in Python: ``percentile_cont`` interpolates linearly between the
two nearest ranks, ``percentile_disc`` returns the first value whose cumulative
fraction ≥ the target, and ``mode`` returns the most frequent value (smallest on a
tie). Works grouped and whole-table.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
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


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


@pytest.fixture
def t(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, dept text, x int)")
    run(
        storage,
        session,
        "INSERT INTO t VALUES (1,'a',1),(2,'a',2),(3,'a',3),(4,'a',4),"
        "(5,'b',10),(6,'b',20),(7,'b',20)",
    )
    return storage


# -- whole-table -------------------------------------------------------------- #


def test_percentile_cont_median(t, session):
    # 7 values sorted (1,2,3,4,10,20,20); rank = 0.5*6 = 3.0 -> vals[3] = 4.
    assert run(t, session, "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY x) FROM t").rows == [
        (4.0,)
    ]


def test_percentile_cont_interpolates(storage, session):
    run(storage, session, "CREATE TABLE u (id int PRIMARY KEY, x int)")
    run(storage, session, "INSERT INTO u VALUES (1,1),(2,2),(3,3),(4,4)")
    # rank = 0.5*3 = 1.5 -> 2 + (3-2)*0.5 = 2.5
    assert run(
        storage, session, "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY x) FROM u"
    ).rows == [(2.5,)]


def test_percentile_cont_bounds(t, session):
    assert run(t, session, "SELECT percentile_cont(0) WITHIN GROUP (ORDER BY x) FROM t").rows == [
        (1.0,)
    ]
    assert run(t, session, "SELECT percentile_cont(1) WITHIN GROUP (ORDER BY x) FROM t").rows == [
        (20.0,)
    ]


def test_percentile_disc(t, session):
    # ceil(0.5*7) = 4 -> idx 3 -> 4.
    assert run(t, session, "SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY x) FROM t").rows == [
        (4,)
    ]


def test_percentile_disc_high(t, session):
    assert run(t, session, "SELECT percentile_disc(0.9) WITHIN GROUP (ORDER BY x) FROM t").rows == [
        (20,)
    ]


def test_mode(t, session):
    assert run(t, session, "SELECT mode() WITHIN GROUP (ORDER BY x) FROM t").rows == [(20,)]


def test_percentile_disc_preserves_int_type(t, session):
    # percentile_disc returns an element of the set — an int stays an int.
    cols = run(t, session, "SELECT percentile_disc(0.5) WITHIN GROUP (ORDER BY x) FROM t").columns
    assert cols[0].type_tag == "int4"


def test_percentile_cont_is_float(t, session):
    cols = run(t, session, "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY x) FROM t").columns
    assert cols[0].type_tag == "float8"


# -- grouped ------------------------------------------------------------------ #


def test_percentile_cont_grouped(t, session):
    rows = run(
        t,
        session,
        "SELECT dept, percentile_cont(0.5) WITHIN GROUP (ORDER BY x) FROM t "
        "GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 2.5), ("b", 20.0)]


def test_mode_grouped(t, session):
    rows = run(
        t,
        session,
        "SELECT dept, mode() WITHIN GROUP (ORDER BY x) FROM t GROUP BY dept ORDER BY dept",
    ).rows
    # dept a is all distinct -> the smallest value; dept b -> 20 (appears twice).
    assert rows == [("a", 1), ("b", 20)]


def test_alongside_plain_aggregate(t, session):
    rows = run(
        t,
        session,
        "SELECT dept, count(*), percentile_cont(0.5) WITHIN GROUP (ORDER BY x) "
        "FROM t GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 4, 2.5), ("b", 3, 20.0)]


# -- NULLs / empty ------------------------------------------------------------ #


def test_nulls_ignored(storage, session):
    run(storage, session, "CREATE TABLE n (id int PRIMARY KEY, x int)")
    run(storage, session, "INSERT INTO n VALUES (1,1),(2,NULL),(3,3)")
    # NULL is dropped: median of {1, 3} -> 2.0
    assert run(
        storage, session, "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY x) FROM n"
    ).rows == [(2.0,)]


def test_all_null_group_is_null(storage, session):
    # A group whose ORDER BY values are all NULL yields NULL (the set is empty
    # after dropping NULLs, but the group row still exists).
    run(storage, session, "CREATE TABLE g (id int PRIMARY KEY, k text, x int)")
    run(storage, session, "INSERT INTO g VALUES (1,'a',NULL),(2,'a',NULL),(3,'b',5)")
    rows = run(
        storage,
        session,
        "SELECT k, percentile_cont(0.5) WITHIN GROUP (ORDER BY x) FROM g GROUP BY k ORDER BY k",
    ).rows
    assert rows == [("a", None), ("b", 5.0)]


# -- errors ------------------------------------------------------------------- #


def test_fraction_out_of_range(t, session):
    assert sqlstate(t, session, "SELECT percentile_cont(1.5) WITHIN GROUP (ORDER BY x) FROM t") == (
        "2202E"
    )


def test_negative_fraction(t, session):
    assert (
        sqlstate(t, session, "SELECT percentile_disc(-0.1) WITHIN GROUP (ORDER BY x) FROM t")
        == "2202E"
    )
