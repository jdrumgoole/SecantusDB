"""Statistical + bitwise aggregates (#102): stddev / stddev_pop / stddev_samp,
variance / var_pop, and bit_and / bit_or / bit_xor (every() already aliases
bool_and). Lowered through the $group pipeline: stddev via Mongo's native
$stdDevPop / $stdDevSamp, variance as its square, bit ops via $push + a Python
fold.
"""

from __future__ import annotations

import functools
import statistics

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"

# (id, x, n, g)
ROWS = [(1, 2.0, 6, 1), (2, 4.0, 3, 1), (3, 4.0, 12, 2), (4, 4.0, 5, 2), (5, 5.0, 10, 2)]
XS = [r[1] for r in ROWS]
NS = [r[2] for r in ROWS]


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
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, x float8, n int, g int)")
    for i, x, n, g in ROWS:
        run(storage, session, f"INSERT INTO t VALUES ({i}, {x}, {n}, {g})")
    return storage


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


def col(storage, session, sql):
    return run(storage, session, sql).columns[0]


# -- stddev ------------------------------------------------------------------- #


def test_stddev_pop(t, session):
    assert val(t, session, "SELECT stddev_pop(x) FROM t") == pytest.approx(statistics.pstdev(XS))


def test_stddev_samp(t, session):
    assert val(t, session, "SELECT stddev_samp(x) FROM t") == pytest.approx(statistics.stdev(XS))


def test_stddev_is_sample(t, session):
    # Bare stddev() is sample stddev in Postgres.
    assert val(t, session, "SELECT stddev(x) FROM t") == pytest.approx(statistics.stdev(XS))


def test_stddev_type_float8(t, session):
    assert col(t, session, "SELECT stddev(x) FROM t").type_tag == "float8"


def test_stddev_samp_single_row_is_null(storage, session):
    run(storage, session, "CREATE TABLE one (id int PRIMARY KEY, x float8)")
    run(storage, session, "INSERT INTO one VALUES (1, 5.0)")
    assert val(storage, session, "SELECT stddev_samp(x) FROM one") is None
    assert val(storage, session, "SELECT stddev_pop(x) FROM one") == 0.0


# -- variance ----------------------------------------------------------------- #


def test_var_pop(t, session):
    assert float(val(t, session, "SELECT var_pop(x) FROM t")) == pytest.approx(
        statistics.pvariance(XS)
    )


def test_variance_is_sample(t, session):
    assert float(val(t, session, "SELECT variance(x) FROM t")) == pytest.approx(
        statistics.variance(XS)
    )


def test_variance_type_numeric(t, session):
    assert col(t, session, "SELECT variance(x) FROM t").type_tag == "numeric"


# -- bitwise ------------------------------------------------------------------ #


def test_bit_and(t, session):
    assert val(t, session, "SELECT bit_and(n) FROM t") == functools.reduce(lambda a, b: a & b, NS)


def test_bit_or(t, session):
    assert val(t, session, "SELECT bit_or(n) FROM t") == functools.reduce(lambda a, b: a | b, NS)


def test_bit_xor(t, session):
    assert val(t, session, "SELECT bit_xor(n) FROM t") == functools.reduce(lambda a, b: a ^ b, NS)


def test_bit_type_int(t, session):
    assert col(t, session, "SELECT bit_and(n) FROM t").type_tag in ("int4", "int8")


# -- grouped + misc ----------------------------------------------------------- #


def test_grouped_bit_or(t, session):
    rows = run(t, session, "SELECT g, bit_or(n) FROM t GROUP BY g ORDER BY g").rows
    assert rows == [(1, 3 | 6), (2, 12 | 5 | 10)]


def test_grouped_stddev(t, session):
    rows = run(t, session, "SELECT g, var_pop(x) FROM t GROUP BY g ORDER BY g").rows
    g1 = statistics.pvariance([2.0, 4.0])
    g2 = statistics.pvariance([4.0, 4.0, 5.0])
    assert float(rows[0][1]) == pytest.approx(g1)
    assert float(rows[1][1]) == pytest.approx(g2)


def test_every_aliases_bool_and(storage, session):
    # every() is the standard-SQL spelling of bool_and() over a boolean column.
    run(storage, session, "CREATE TABLE b (id int PRIMARY KEY, flag bool)")
    run(storage, session, "INSERT INTO b VALUES (1, true), (2, true)")
    assert val(storage, session, "SELECT every(flag) FROM b") is True
    run(storage, session, "INSERT INTO b VALUES (3, false)")
    assert val(storage, session, "SELECT every(flag) FROM b") is False


def test_null_values_skipped(storage, session):
    # NULL values are ignored by the aggregates (Postgres semantics).
    run(storage, session, "CREATE TABLE nn (id int PRIMARY KEY, x float8, n int)")
    run(storage, session, "INSERT INTO nn VALUES (1, 4.0, 6), (2, NULL, NULL), (3, 6.0, 3)")
    assert val(storage, session, "SELECT stddev_pop(x) FROM nn") == pytest.approx(1.0)
    assert val(storage, session, "SELECT bit_and(n) FROM nn") == (6 & 3)
