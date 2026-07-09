"""In-call ``ORDER BY`` for ``array_agg`` / ``string_agg`` over a JOIN (#170).

``array_agg(x ORDER BY y)`` / ``string_agg(x, sep ORDER BY y)`` collect a
``{value, sort-key}`` pair per row and sort them in Python before building the
array / joining the string — now resolved through the join resolver so the ORDER
BY keys may name joined columns. Driven through ``run_sql`` over the real
WiredTiger-backed ``Storage``.
"""

from __future__ import annotations

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
    run(storage, session, "CREATE TABLE ord (id int primary key, cust text, prod text, qty int)")
    run(storage, session, "INSERT INTO cust VALUES ('a', 'e'), ('b', 'e')")
    # products chosen so alphabetical order (apple<banana<cherry) differs from
    # qty-descending order (cherry@9, apple@5, banana@2).
    data = [("a", "banana", 2), ("a", "apple", 5), ("b", "cherry", 9)]
    for i, (c, p, q) in enumerate(data):
        run(storage, session, f"INSERT INTO ord VALUES ({i}, '{c}', '{p}', {q})")


_J = "FROM ord o JOIN cust c ON o.cust = c.name GROUP BY c.region"


def test_array_agg_order_by_value(storage, session):
    _seed(storage, session)
    r = rows(storage, session, f"SELECT c.region, array_agg(o.prod ORDER BY o.prod) AS a {_J}")
    assert r == [("e", ["apple", "banana", "cherry"])]


def test_array_agg_order_by_other_column_desc(storage, session):
    _seed(storage, session)
    # ORDER BY qty DESC → cherry(9), apple(5), banana(2) — distinct from name order.
    r = rows(storage, session, f"SELECT c.region, array_agg(o.prod ORDER BY o.qty DESC) AS a {_J}")
    assert r == [("e", ["cherry", "apple", "banana"])]


def test_string_agg_order_by(storage, session):
    _seed(storage, session)
    r = rows(
        storage, session, f"SELECT c.region, string_agg(o.prod, ',' ORDER BY o.prod) AS a {_J}"
    )
    assert r == [("e", "apple,banana,cherry")]
