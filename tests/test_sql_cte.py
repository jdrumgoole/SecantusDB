"""Non-recursive common table expressions: ``WITH name AS (...) <query>``.

Each CTE is materialized to rows and registered as an ephemeral collection, then
the main query resolves the CTE name like any table — so CTEs compose with the
single-table, JOIN/GROUP, and set-operation paths. CTEs materialize in order, so
a later one may reference an earlier one.
"""

from __future__ import annotations

from decimal import Decimal

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
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE sales (id bigint primary key, region text, amount int)")
    rows = [(1, "east", 10), (2, "east", 20), (3, "west", 30), (4, "west", 5)]
    for i, r, a in rows:
        s.q(f"INSERT INTO sales (id, region, amount) VALUES ({i}, '{r}', {a})")
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def test_basic_cte(storage, session):
    res = q(
        storage,
        session,
        "WITH big AS (SELECT * FROM sales WHERE amount >= 20) "
        "SELECT region, amount FROM big ORDER BY amount",
    )
    assert res.rows == [("east", 20), ("west", 30)]


def test_cte_select_star(storage, session):
    res = q(
        storage,
        session,
        "WITH t AS (SELECT region, amount FROM sales WHERE id = 1) SELECT * FROM t",
    )
    assert [c.name for c in res.columns] == ["region", "amount"]
    assert res.rows == [("east", 10)]


def test_cte_then_filter_in_main(storage, session):
    res = q(
        storage,
        session,
        "WITH t AS (SELECT region, amount FROM sales) "
        "SELECT region FROM t WHERE amount > 15 ORDER BY region",
    )
    assert res.rows == [("east",), ("west",)]


def test_cte_feeds_group_by(storage, session):
    res = q(
        storage,
        session,
        "WITH t AS (SELECT region, amount FROM sales) "
        "SELECT region, SUM(amount) AS total FROM t GROUP BY region ORDER BY region",
    )
    assert res.rows == [("east", 30), ("west", 35)]


def test_cte_with_aggregate_inner(storage, session):
    # The CTE itself aggregates; the main query filters the aggregated rows.
    res = q(
        storage,
        session,
        "WITH totals AS (SELECT region, SUM(amount) AS total FROM sales GROUP BY region) "
        "SELECT region FROM totals WHERE total > 31 ORDER BY region",
    )
    assert res.rows == [("west",)]


def test_multiple_ctes(storage, session):
    res = q(
        storage,
        session,
        "WITH e AS (SELECT amount FROM sales WHERE region = 'east'), "
        "w AS (SELECT amount FROM sales WHERE region = 'west') "
        "SELECT amount FROM e UNION SELECT amount FROM w ORDER BY amount",
    )
    assert res.rows == [(5,), (10,), (20,), (30,)]


def test_chained_cte_references_earlier(storage, session):
    res = q(
        storage,
        session,
        "WITH a AS (SELECT region, amount FROM sales), "
        "b AS (SELECT region, amount FROM a WHERE amount >= 20) "
        "SELECT region, amount FROM b ORDER BY amount",
    )
    assert res.rows == [("east", 20), ("west", 30)]


def test_cte_in_join(storage, session):
    storage.q("CREATE TABLE region_info (id bigint primary key, region text, label text)")
    storage.q("INSERT INTO region_info (id, region, label) VALUES (1, 'east', 'East Coast')")
    storage.q("INSERT INTO region_info (id, region, label) VALUES (2, 'west', 'West Coast')")
    res = q(
        storage,
        session,
        "WITH t AS (SELECT region, amount FROM sales WHERE amount >= 20) "
        "SELECT r.label, t.amount FROM t JOIN region_info r ON t.region = r.region "
        "ORDER BY t.amount",
    )
    assert res.rows == [("East Coast", 20), ("West Coast", 30)]


def test_cte_in_set_operation_main(storage, session):
    res = q(
        storage,
        session,
        "WITH t AS (SELECT amount FROM sales WHERE region = 'east') "
        "SELECT amount FROM t UNION SELECT amount FROM sales WHERE region = 'west' ORDER BY amount",
    )
    assert res.rows == [(5,), (10,), (20,), (30,)]


def test_empty_cte_yields_no_rows(storage, session):
    res = q(
        storage,
        session,
        "WITH none AS (SELECT region, amount FROM sales WHERE amount > 1000) SELECT * FROM none",
    )
    assert res.rows == []


def test_cte_preserves_numeric_type(storage, session):
    storage.q("CREATE TABLE prices (id bigint primary key, p numeric)")
    storage.q("INSERT INTO prices (id, p) VALUES (1, 19.99)")
    res = q(storage, session, "WITH t AS (SELECT p FROM prices) SELECT p FROM t")
    assert res.rows == [(Decimal("19.99"),)]


def test_cte_does_not_leak_across_statements(storage, session):
    # A CTE name is scoped to its statement; a later statement must not see it.
    q(storage, session, "WITH scoped AS (SELECT * FROM sales) SELECT count(*) FROM scoped")
    from secantus.sql import SQLError

    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT * FROM scoped")
    assert ei.value.sqlstate == "42P01"
