"""P5 tests: JOIN, GROUP BY, and aggregate functions.

End-to-end through ``run_sql`` over the real WT-backed ``Storage``, so the
SQL→pipeline translation is exercised against the same `$group`/`$lookup` the
production server uses.
"""

from __future__ import annotations

from decimal import Decimal

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
    try:
        yield s
    finally:
        s.close()


def q(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[0]


def _sales(storage, session):
    q(storage, session, "CREATE TABLE sales (id bigint primary key, region text, amount int)")
    rows = [(1, "east", 10), (2, "east", 20), (3, "west", 30), (4, "west", 5)]
    for i, r, a in rows:
        q(storage, session, f"INSERT INTO sales (id, region, amount) VALUES ({i}, '{r}', {a})")


# -- aggregates / GROUP BY --------------------------------------------------- #


def test_group_by_with_all_aggregates(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, COUNT(*) AS n, SUM(amount) AS s, AVG(amount) AS a, "
        "MIN(amount) AS lo, MAX(amount) AS hi FROM sales GROUP BY region ORDER BY region",
    )
    assert [c.name for c in res.columns] == ["region", "n", "s", "a", "lo", "hi"]
    assert res.rows == [
        ("east", 2, 30, 15.0, 10, 20),
        ("west", 2, 35, 17.5, 5, 30),
    ]


def test_whole_table_aggregate_no_group(storage, session):
    _sales(storage, session)
    res = q(storage, session, "SELECT COUNT(*), SUM(amount), AVG(amount) FROM sales")
    assert [c.name for c in res.columns] == ["count", "sum", "avg"]
    assert res.rows == [(4, 65, 16.25)]


def test_count_column_excludes_nulls(storage, session):
    q(storage, session, "CREATE TABLE t (id bigint primary key, v int)")
    q(storage, session, "INSERT INTO t (id, v) VALUES (1, 5), (2, NULL), (3, 9)")
    res = q(storage, session, "SELECT COUNT(*) AS all_rows, COUNT(v) AS non_null FROM t")
    assert res.rows == [(3, 2)]


def test_where_then_group(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, SUM(amount) AS s FROM sales WHERE amount >= 10 "
        "GROUP BY region ORDER BY region",
    )
    assert res.rows == [("east", 30), ("west", 30)]


def test_having_filters_groups(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, SUM(amount) AS s FROM sales GROUP BY region HAVING SUM(amount) > 30",
    )
    assert res.rows == [("west", 35)]


def test_having_references_unselected_aggregate(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, COUNT(*) AS n FROM sales GROUP BY region "
        "HAVING SUM(amount) > 25 ORDER BY region",
    )
    assert res.rows == [("east", 2), ("west", 2)]


def test_group_order_by_aggregate_limit(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, SUM(amount) AS s FROM sales GROUP BY region ORDER BY s DESC LIMIT 1",
    )
    assert res.rows == [("west", 35)]


def test_non_grouped_column_is_rejected(storage, session):
    _sales(storage, session)
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT region, amount FROM sales GROUP BY region")
    assert ei.value.sqlstate == "42803"


def test_numeric_sum_returns_decimal(storage, session):
    q(storage, session, "CREATE TABLE p (id bigint primary key, price numeric)")
    q(storage, session, "INSERT INTO p (id, price) VALUES (1, 1.50), (2, 2.25)")
    res = q(storage, session, "SELECT SUM(price) AS total FROM p")
    assert res.rows == [(Decimal("3.75"),)]


# -- JOIN -------------------------------------------------------------------- #


def _orders(storage, session):
    q(storage, session, "CREATE TABLE customers (id bigint primary key, name text, region text)")
    q(storage, session, "CREATE TABLE orders (id bigint primary key, cust_id bigint, total int)")
    q(
        storage,
        session,
        "INSERT INTO customers (id, name, region) VALUES (1,'alice','e'),(2,'bob','w')",
    )
    q(
        storage,
        session,
        "INSERT INTO orders (id, cust_id, total) VALUES (10,1,100),(11,1,200),(12,2,50),(13,9,999)",
    )


def test_inner_join_drops_unmatched(storage, session):
    _orders(storage, session)
    res = q(
        storage,
        session,
        "SELECT o.id, o.total, c.name FROM orders o "
        "JOIN customers c ON o.cust_id = c.id ORDER BY o.id",
    )
    assert [c.name for c in res.columns] == ["id", "total", "name"]
    # order 13 (cust_id 9) has no matching customer.
    assert res.rows == [(10, 100, "alice"), (11, 200, "alice"), (12, 50, "bob")]


def test_left_join_keeps_unmatched_as_null(storage, session):
    _orders(storage, session)
    res = q(
        storage,
        session,
        "SELECT o.id, c.name FROM orders o LEFT JOIN customers c ON o.cust_id = c.id ORDER BY o.id",
    )
    assert res.rows == [(10, "alice"), (11, "alice"), (12, "bob"), (13, None)]


def test_join_with_where_on_joined_table(storage, session):
    _orders(storage, session)
    res = q(
        storage,
        session,
        "SELECT o.id, c.name FROM orders o JOIN customers c ON o.cust_id = c.id "
        "WHERE c.region = 'e' ORDER BY o.id",
    )
    assert res.rows == [(10, "alice"), (11, "alice")]


def test_join_ambiguous_column_rejected(storage, session):
    _orders(storage, session)
    with pytest.raises(SQLError) as ei:
        # ``id`` exists in both tables.
        q(storage, session, "SELECT id FROM orders o JOIN customers c ON o.cust_id = c.id")
    assert ei.value.sqlstate == "42702"


def test_join_unknown_alias_rejected(storage, session):
    _orders(storage, session)
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT x.id FROM orders o JOIN customers c ON o.cust_id = c.id")
    assert ei.value.sqlstate == "42P01"


# -- multi-table joins ------------------------------------------------------- #


def _orders_products(storage, session):
    q(storage, session, "CREATE TABLE customers (id bigint primary key, name text)")
    q(storage, session, "CREATE TABLE products (id bigint primary key, pname text)")
    q(
        storage,
        session,
        "CREATE TABLE orders (id bigint primary key, cust_id bigint, prod_id bigint)",
    )
    q(storage, session, "INSERT INTO customers (id, name) VALUES (1,'alice'),(2,'bob')")
    q(storage, session, "INSERT INTO products (id, pname) VALUES (100,'gear'),(101,'widget')")
    q(
        storage,
        session,
        "INSERT INTO orders (id, cust_id, prod_id) VALUES (10,1,100),(11,1,101),(12,2,100)",
    )


def test_three_table_join(storage, session):
    _orders_products(storage, session)
    res = q(
        storage,
        session,
        "SELECT c.name, p.pname FROM orders o "
        "JOIN customers c ON o.cust_id = c.id "
        "JOIN products p ON o.prod_id = p.id ORDER BY c.name, p.pname",
    )
    assert [c.name for c in res.columns] == ["name", "pname"]
    assert res.rows == [("alice", "gear"), ("alice", "widget"), ("bob", "gear")]


def test_three_table_join_with_where(storage, session):
    _orders_products(storage, session)
    res = q(
        storage,
        session,
        "SELECT c.name, p.pname FROM orders o "
        "JOIN customers c ON o.cust_id = c.id "
        "JOIN products p ON o.prod_id = p.id "
        "WHERE p.pname = 'gear' ORDER BY c.name",
    )
    assert res.rows == [("alice", "gear"), ("bob", "gear")]


def test_join_chained_on_previous_table(storage, session):
    # The third table joins on the second (a→b, b→c), not on the base.
    q(storage, session, "CREATE TABLE a (id bigint primary key, b_id bigint)")
    q(storage, session, "CREATE TABLE b (id bigint primary key, c_id bigint)")
    q(storage, session, "CREATE TABLE c (id bigint primary key, label text)")
    q(storage, session, "INSERT INTO a (id, b_id) VALUES (1, 10)")
    q(storage, session, "INSERT INTO b (id, c_id) VALUES (10, 100)")
    q(storage, session, "INSERT INTO c (id, label) VALUES (100, 'deep')")
    res = q(
        storage,
        session,
        "SELECT c.label FROM a JOIN b ON a.b_id = b.id JOIN c ON b.c_id = c.id",
    )
    assert res.rows == [("deep",)]


def test_join_with_disconnected_table_rejected(storage, session):
    _orders_products(storage, session)
    with pytest.raises(SQLError) as ei:
        # The second join's ON relates two not-yet-known aliases.
        q(
            storage,
            session,
            "SELECT c.name FROM orders o JOIN customers c ON o.cust_id = c.id "
            "JOIN products p ON p.id = nope.x",
        )
    assert ei.value.sqlstate in ("0A000", "42P01")


# -- SELECT DISTINCT --------------------------------------------------------- #


def test_distinct_single_column(storage, session):
    _sales(storage, session)
    res = q(storage, session, "SELECT DISTINCT region FROM sales ORDER BY region")
    assert res.rows == [("east",), ("west",)]


def test_distinct_multi_column(storage, session):
    _sales(storage, session)
    q(storage, session, "INSERT INTO sales (id, region, amount) VALUES (5, 'east', 10)")
    # (east,10) appears twice (ids 1 and 5) — DISTINCT collapses it.
    res = q(
        storage,
        session,
        "SELECT DISTINCT region, amount FROM sales ORDER BY region, amount",
    )
    assert res.rows == [("east", 10), ("east", 20), ("west", 5), ("west", 30)]


def test_distinct_over_join(storage, session):
    _orders(storage, session)
    # alice has two orders; DISTINCT on her name collapses to one row.
    res = q(
        storage,
        session,
        "SELECT DISTINCT c.name FROM orders o JOIN customers c ON o.cust_id = c.id ORDER BY c.name",
    )
    assert res.rows == [("alice",), ("bob",)]


# -- DISTINCT aggregate inside HAVING (#166) --------------------------------- #


def _grp(storage, session):
    q(storage, session, "CREATE TABLE t (id int primary key, g text, x int)")
    for i, (g, x) in enumerate([("a", 1), ("a", 1), ("a", 2), ("b", 5), ("b", 5), ("c", 9)]):
        q(storage, session, f"INSERT INTO t VALUES ({i}, '{g}', {x})")


def test_having_count_distinct(storage, session):
    _grp(storage, session)
    # a has distinct {1,2}=2; b {5}=1; c {9}=1 → only a qualifies.
    res = q(
        storage,
        session,
        "SELECT g FROM t GROUP BY g HAVING count(DISTINCT x) > 1 ORDER BY g",
    )
    assert res.rows == [("a",)]


def test_having_sum_distinct(storage, session):
    _grp(storage, session)
    # distinct sums: a=1+2=3, b=5, c=9 → all >= 3.
    res = q(
        storage,
        session,
        "SELECT g FROM t GROUP BY g HAVING sum(DISTINCT x) >= 3 ORDER BY g",
    )
    assert res.rows == [("a",), ("b",), ("c",)]


def test_having_distinct_reuses_select_aggregate(storage, session):
    _grp(storage, session)
    res = q(
        storage,
        session,
        "SELECT g, count(DISTINCT x) AS c FROM t GROUP BY g "
        "HAVING count(DISTINCT x) > 1 ORDER BY g",
    )
    assert res.rows == [("a", 2)]


# -- ORDER BY position / aggregate in grouped queries ------------------------ #


def test_order_by_position_grouped(storage, session):
    # ORDER BY 1 = the first select column (region); ORDER BY 2 = the count.
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, count(*) FROM sales GROUP BY region ORDER BY 1",
    )
    assert res.rows == [("east", 2), ("west", 2)]


def test_order_by_position_desc_with_tiebreak(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, sum(amount) FROM sales GROUP BY region ORDER BY 2 DESC, 1",
    )
    # east sum=30, west sum=35 → west first.
    assert res.rows == [("west", 35), ("east", 30)]


def test_order_by_aggregate_expression(storage, session):
    # ORDER BY count(*) resolves to the matching select-list aggregate output.
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, sum(amount) FROM sales GROUP BY region ORDER BY sum(amount) DESC",
    )
    assert res.rows == [("west", 35), ("east", 30)]


def test_order_by_position_out_of_range(storage, session):
    _sales(storage, session)
    with pytest.raises(SQLError) as ei:
        q(storage, session, "SELECT region, count(*) FROM sales GROUP BY region ORDER BY 5")
    assert ei.value.sqlstate == "42P10"


def test_order_by_position_over_join(storage, session):
    _sales(storage, session)
    q(storage, session, "CREATE TABLE reg (region text primary key, tz text)")
    q(storage, session, "INSERT INTO reg (region, tz) VALUES ('east', 'ET'), ('west', 'PT')")
    res = q(
        storage,
        session,
        "SELECT r.tz, sum(s.amount) FROM sales s JOIN reg r ON s.region = r.region "
        "GROUP BY r.tz ORDER BY 2 DESC",
    )
    assert res.rows == [("PT", 35), ("ET", 30)]


def test_order_by_position_rollup(storage, session):
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, count(*) FROM sales GROUP BY ROLLUP(region) ORDER BY 1",
    )
    # ORDER BY 1 sorts region; the grand-total row (NULL region) sorts last.
    assert res.rows == [("east", 2), ("west", 2), (None, 4)]


# -- ORDER BY an aggregate not in the select list ---------------------------- #


def test_order_by_unselected_aggregate(storage, session):
    # ORDER BY sum(amount) with only region selected: sorts by the hidden sum,
    # and the sum does not appear in the output.
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region FROM sales GROUP BY region ORDER BY sum(amount) DESC",
    )
    # west sum 35 > east sum 30; only the selected column is returned.
    assert len(res.columns) == 1
    assert res.rows == [("west",), ("east",)]


def test_order_by_unselected_count(storage, session):
    _sales(storage, session)
    # east and west each have 2 rows → tie on count(*), broken by region asc.
    res = q(
        storage,
        session,
        "SELECT region FROM sales GROUP BY region ORDER BY count(*) DESC, region",
    )
    assert res.rows == [("east",), ("west",)]
    assert len(res.columns) == 1


def test_order_by_unselected_aggregate_with_selected_agg(storage, session):
    # A selected aggregate plus ordering by a *different*, unselected aggregate.
    _sales(storage, session)
    res = q(
        storage,
        session,
        "SELECT region, count(*) FROM sales GROUP BY region ORDER BY sum(amount) DESC",
    )
    assert res.rows == [("west", 2), ("east", 2)]


def test_order_by_unselected_count_distinct(storage, session):
    _sales(storage, session)
    # east amounts {10,20} → 2 distinct; west {30,5} → 2; tie, region asc.
    res = q(
        storage,
        session,
        "SELECT region FROM sales GROUP BY region ORDER BY count(DISTINCT amount) DESC, region",
    )
    assert res.rows == [("east",), ("west",)]


def test_order_by_unselected_aggregate_over_join(storage, session):
    _sales(storage, session)
    q(storage, session, "CREATE TABLE reg (region text PRIMARY KEY, tz text)")
    q(storage, session, "INSERT INTO reg (region, tz) VALUES ('east', 'ET'), ('west', 'PT')")
    res = q(
        storage,
        session,
        "SELECT r.tz FROM sales s JOIN reg r ON s.region = r.region "
        "GROUP BY r.tz ORDER BY sum(s.amount) DESC",
    )
    # west sum 35 (PT) > east sum 30 (ET).
    assert res.rows == [("PT",), ("ET",)]
    assert len(res.columns) == 1
