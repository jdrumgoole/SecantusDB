"""Statistical / bitwise / boolean aggregates over a JOIN (#169).

``variance`` / ``var_pop`` (a post-``$group`` square of the corresponding
``$stdDev``), the bitwise reductions ``bit_and`` / ``bit_or`` / ``bit_xor`` (a
``$push`` + Python fold), and ``bool_and`` / ``bool_or`` / ``every`` all work over
a JOIN + GROUP BY, resolving each aggregate's argument through the join resolver.
Driven through ``run_sql`` over the real WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

import statistics

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


def run(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1]


def rows(storage, session, sql):
    return run(storage, session, sql).rows


def _seed(storage, session):
    run(storage, session, "CREATE TABLE cust (name text primary key, region text)")
    run(storage, session, "CREATE TABLE ord (id int primary key, cust text, amt int, ok boolean)")
    run(storage, session, "INSERT INTO cust VALUES ('a', 'e'), ('b', 'e'), ('c', 'w')")
    # region e (cust a, b): amts 4, 8, 2 ; region w (cust c): amt 30
    data = [("a", 4, True), ("a", 8, True), ("b", 2, False), ("c", 30, True)]
    for i, (c, a, ok) in enumerate(data):
        run(storage, session, f"INSERT INTO ord VALUES ({i}, '{c}', {a}, {str(ok).lower()})")


_J = "FROM ord o JOIN cust c ON o.cust = c.name GROUP BY c.region ORDER BY c.region"


def test_join_variance(storage, session):
    _seed(storage, session)
    r = rows(storage, session, f"SELECT c.region, variance(o.amt) AS v {_J}")
    # sample variance of {4,8,2}; a single sample (w) → NULL.
    assert r[0][0] == "e" and r[0][1] == pytest.approx(statistics.variance([4, 8, 2]))
    assert r[1] == ("w", None)


def test_join_var_pop(storage, session):
    _seed(storage, session)
    r = rows(storage, session, f"SELECT c.region, var_pop(o.amt) AS v {_J}")
    assert r[0][0] == "e" and r[0][1] == pytest.approx(statistics.pvariance([4, 8, 2]))
    assert r[1] == ("w", pytest.approx(0.0))


def test_join_bit_and_or_xor(storage, session):
    _seed(storage, session)
    r = rows(
        storage,
        session,
        "SELECT c.region, bit_and(o.amt) AS a, bit_or(o.amt) AS o1, bit_xor(o.amt) AS x " + _J,
    )
    assert r == [
        ("e", 4 & 8 & 2, 4 | 8 | 2, 4 ^ 8 ^ 2),
        ("w", 30, 30, 30),
    ]


def test_join_bool_and_or_every(storage, session):
    _seed(storage, session)
    r = rows(
        storage,
        session,
        "SELECT c.region, bool_and(o.ok) AS a, bool_or(o.ok) AS o1, every(o.ok) AS e " + _J,
    )
    # region e: ok = {T, T, F} → and F, or T, every F ; region w: {T} → all T.
    assert r == [("e", False, True, False), ("w", True, True, True)]
