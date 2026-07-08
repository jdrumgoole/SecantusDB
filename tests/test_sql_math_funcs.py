"""Math / numeric scalar functions: ``trunc`` / ``sqrt`` / ``cbrt`` / ``sign`` /
``ln`` / ``log`` / ``log10`` / ``exp`` / ``pi`` / ``degrees`` / ``radians`` /
``factorial`` / ``gcd`` / ``lcm`` (evaluated per row in ``secantus.sql.scalar``).
"""

from __future__ import annotations

import math

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
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, x double precision, n int)")
    run(storage, session, "INSERT INTO t VALUES (1, 9.87, 5), (2, -27.0, 12), (3, 16.0, 18)")
    return storage


def val(storage, session, sql):
    return run(storage, session, sql).rows[0][0]


# -- trunc -------------------------------------------------------------------- #


def test_trunc_default(t, session):
    assert val(t, session, "SELECT trunc(x) FROM t WHERE id=1") == 9


def test_trunc_decimals(t, session):
    assert val(t, session, "SELECT trunc(x, 1) FROM t WHERE id=1") == pytest.approx(9.8)


def test_trunc_toward_zero_negative(t, session):
    assert val(t, session, "SELECT trunc(x) FROM t WHERE id=2") == -27


# -- sqrt / cbrt -------------------------------------------------------------- #


def test_sqrt(t, session):
    assert val(t, session, "SELECT sqrt(x) FROM t WHERE id=3") == pytest.approx(4.0)


def test_cbrt_negative(t, session):
    assert val(t, session, "SELECT cbrt(x) FROM t WHERE id=2") == pytest.approx(-3.0)


# -- sign --------------------------------------------------------------------- #


def test_sign_negative(t, session):
    assert val(t, session, "SELECT sign(x) FROM t WHERE id=2") == -1


def test_sign_positive(t, session):
    assert val(t, session, "SELECT sign(x) FROM t WHERE id=1") == 1


# -- ln / log / log10 --------------------------------------------------------- #


def test_ln(t, session):
    assert val(t, session, "SELECT ln(exp(1)) FROM t WHERE id=1") == pytest.approx(1.0)


def test_log_base10_default(t, session):
    assert val(t, session, "SELECT log(1000.0) FROM t WHERE id=1") == pytest.approx(3.0)


def test_log_explicit_base(t, session):
    assert val(t, session, "SELECT log(2, 8) FROM t WHERE id=1") == pytest.approx(3.0)


def test_log10_exact(t, session):
    # log10(1000) must be exactly 3.0, not 2.9999999999999996.
    assert val(t, session, "SELECT log10(1000.0) FROM t WHERE id=1") == 3.0


# -- exp / pi / degrees / radians --------------------------------------------- #


def test_exp(t, session):
    assert val(t, session, "SELECT exp(1) FROM t WHERE id=1") == pytest.approx(math.e)


def test_pi(t, session):
    assert val(t, session, "SELECT pi() FROM t WHERE id=1") == pytest.approx(math.pi)


def test_degrees(t, session):
    assert val(t, session, "SELECT degrees(pi()) FROM t WHERE id=1") == pytest.approx(180.0)


def test_radians(t, session):
    assert val(t, session, "SELECT radians(180) FROM t WHERE id=1") == pytest.approx(math.pi)


# -- factorial / gcd / lcm ---------------------------------------------------- #


def test_factorial(t, session):
    assert val(t, session, "SELECT factorial(n) FROM t WHERE id=1") == 120


def test_gcd(t, session):
    assert val(t, session, "SELECT gcd(n, 18) FROM t WHERE id=1") == 1


def test_lcm(t, session):
    assert val(t, session, "SELECT lcm(4, 6) FROM t WHERE id=1") == 12


# -- already-supported companions (mod / power / abs / ceil / floor / round) --- #


def test_mod(t, session):
    assert val(t, session, "SELECT mod(10, 3) FROM t WHERE id=1") == 1


def test_power(t, session):
    assert val(t, session, "SELECT power(2, 10) FROM t WHERE id=1") == 1024


# -- NULL propagation --------------------------------------------------------- #


def test_null_propagates(t, session):
    assert val(t, session, "SELECT sqrt(NULL) FROM t WHERE id=1") is None
    assert val(t, session, "SELECT trunc(NULL, 2) FROM t WHERE id=1") is None
    assert val(t, session, "SELECT gcd(NULL, 4) FROM t WHERE id=1") is None
