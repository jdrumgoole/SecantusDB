"""CROSS JOIN, implicit comma-joins, and non-equi / OR join conditions.

A join with no ON (``CROSS JOIN`` or the implicit ``FROM a, b`` comma form)
compiles to a cartesian product: an empty ``$lookup`` pipeline returns every
foreign doc, then ``$unwind`` pairs each with the outer row. Non-equality and
``OR`` join conditions already ride the ``$lookup`` ``let``/``pipeline`` form via
the ON translator; these tests pin that too.
"""

from __future__ import annotations

import pytest

from secantus.sql import SQLError, run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE a (id bigint primary key, av int)")
    s.q("CREATE TABLE b (id bigint primary key, bv int)")
    for i, v in [(1, 10), (2, 20)]:
        s.q(f"INSERT INTO a (id, av) VALUES ({i}, {v})")
    for i, v in [(1, 15), (2, 25)]:
        s.q(f"INSERT INTO b (id, bv) VALUES ({i}, {v})")
    try:
        yield s
    finally:
        s.close()


def rows(storage, session, sql):
    return sorted(
        run_sql(storage, DB, sql, session=session)[0].rows, key=lambda r: tuple(map(str, r))
    )


def test_cross_join(storage, session):
    assert rows(storage, session, "SELECT a.av, b.bv FROM a CROSS JOIN b") == [
        (10, 15),
        (10, 25),
        (20, 15),
        (20, 25),
    ]


def test_comma_join(storage, session):
    assert rows(storage, session, "SELECT a.av, b.bv FROM a, b") == [
        (10, 15),
        (10, 25),
        (20, 15),
        (20, 25),
    ]


def test_comma_join_with_where_is_inner_join(storage, session):
    assert rows(storage, session, "SELECT a.av, b.bv FROM a, b WHERE a.id = b.id") == [
        (10, 15),
        (20, 25),
    ]


def test_three_table_comma_join(storage, session):
    storage.q("CREATE TABLE c (id bigint primary key, cv int)")
    storage.q("INSERT INTO c (id, cv) VALUES (1, 100)")
    assert rows(storage, session, "SELECT a.av, b.bv, c.cv FROM a, b, c WHERE a.id = b.id") == [
        (10, 15, 100),
        (20, 25, 100),
    ]


def test_cross_join_with_group_by(storage, session):
    assert rows(
        storage, session, "SELECT a.id, COUNT(*) AS n FROM a CROSS JOIN b GROUP BY a.id"
    ) == [(1, 2), (2, 2)]


def test_cross_then_inner_join(storage, session):
    storage.q("CREATE TABLE c (id bigint primary key, cv int)")
    storage.q("INSERT INTO c (id, cv) VALUES (1, 100)")
    # a × b, then INNER JOIN c on c.id = a.id (only a.id=1 matches).
    assert rows(
        storage, session, "SELECT a.av, b.bv, c.cv FROM a CROSS JOIN b JOIN c ON c.id = a.id"
    ) == [(10, 15, 100), (10, 25, 100)]


def test_cross_join_scalar_expr(storage, session):
    # Evaluated path: a scalar expression in the SELECT list over a cross product.
    assert rows(storage, session, "SELECT a.av + b.bv AS s FROM a CROSS JOIN b") == [
        (25,),
        (35,),
        (35,),
        (45,),
    ]


def test_non_equi_join_condition(storage, session):
    # a.av < b.bv : 10<15, 10<25, 20<25 (not 20<15).
    assert rows(storage, session, "SELECT a.av, b.bv FROM a JOIN b ON a.av < b.bv") == [
        (10, 15),
        (10, 25),
        (20, 25),
    ]


def test_or_join_condition(storage, session):
    assert rows(
        storage, session, "SELECT a.av, b.bv FROM a JOIN b ON a.id = b.id OR a.av = b.bv"
    ) == [(10, 15), (20, 25)]


def test_left_join_without_on_rejected(storage, session):
    with pytest.raises(SQLError) as ei:
        run_sql(storage, DB, "SELECT * FROM a LEFT JOIN b", session=session)
    assert ei.value.sqlstate == "42601"
