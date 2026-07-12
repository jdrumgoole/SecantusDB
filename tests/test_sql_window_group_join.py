"""Window functions over a JOIN + GROUP BY in one SELECT.

The join analogue of ``test_sql_window_group``: the $lookup/$unwind/$group
pipeline produces the grouped rows (aggregates resolved through the join
resolver), then the evaluated executor computes the windows over them — so a
window's args / PARTITION BY / ORDER BY may reference aggregates of the joined
tables.
"""

from __future__ import annotations

import pytest

from secantus.sql import run_sql
from secantus.sql.session import Session
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE orders (id bigint primary key, cid int, amt int)")
    s.q("CREATE TABLE customers (id bigint primary key, region text)")
    for i, c, a in [(1, 1, 10), (2, 1, 20), (3, 2, 30), (4, 2, 5), (5, 3, 40)]:
        s.q(f"INSERT INTO orders (id, cid, amt) VALUES ({i}, {c}, {a})")
    for i, r in [(1, "e"), (2, "e"), (3, "w")]:
        s.q(f"INSERT INTO customers (id, region) VALUES ({i}, '{r}')")
    try:
        yield s
    finally:
        s.close()


def test_rank_over_join_group_aggregate(storage):
    # SUM(amt) per region over the join: e = 30+35 = 65, w = 40 → ranked desc.
    res = storage.q(
        "SELECT c.region, SUM(o.amt) AS s, RANK() OVER (ORDER BY SUM(o.amt) DESC) AS rk "
        "FROM orders o JOIN customers c ON o.cid = c.id GROUP BY c.region ORDER BY c.region"
    )
    assert res.rows == [("e", 65, 1), ("w", 40, 2)]
    # sum(int) is bigint in Postgres.
    assert [c.type_tag for c in res.columns] == ["text", "int8", "int8"]


def test_window_aggregate_nested_over_join_group(storage):
    res = storage.q(
        "SELECT c.region, SUM(o.amt) AS s, SUM(SUM(o.amt)) OVER () AS total "
        "FROM orders o JOIN customers c ON o.cid = c.id GROUP BY c.region ORDER BY c.region"
    )
    assert res.rows == [("e", 65, 105), ("w", 40, 105)]


def test_row_number_over_join_count_order_by_alias(storage):
    res = storage.q(
        "SELECT c.region, COUNT(*) AS n, ROW_NUMBER() OVER (ORDER BY COUNT(*) DESC) AS rn "
        "FROM orders o JOIN customers c ON o.cid = c.id GROUP BY c.region ORDER BY rn"
    )
    assert res.rows == [("e", 4, 1), ("w", 1, 2)]


def test_partition_by_join_group_column(storage):
    # Add a second grouping dimension so PARTITION BY has something to split on.
    storage.q("CREATE TABLE tier (id bigint primary key, cid int, level text)")
    for i, c, lv in [(1, 1, "gold"), (2, 2, "silver"), (3, 3, "gold")]:
        storage.q(f"INSERT INTO tier (id, cid, level) VALUES ({i}, {c}, '{lv}')")
    res = storage.q(
        "SELECT t.level, c.region, SUM(o.amt) AS s, "
        "RANK() OVER (PARTITION BY t.level ORDER BY SUM(o.amt) DESC) AS rk "
        "FROM orders o JOIN customers c ON o.cid = c.id JOIN tier t ON o.cid = t.cid "
        "GROUP BY t.level, c.region ORDER BY t.level, rk"
    )
    # gold: region e (cid1 = 30), region w (cid3 = 40) → w rank1, e rank2.
    # silver: region e (cid2 = 35) → rank1.
    assert res.rows == [
        ("gold", "w", 40, 1),
        ("gold", "e", 30, 2),
        ("silver", "e", 35, 1),
    ]


def test_having_with_window_over_join_group(storage):
    res = storage.q(
        "SELECT c.region, SUM(o.amt) AS s, RANK() OVER (ORDER BY SUM(o.amt)) AS rk "
        "FROM orders o JOIN customers c ON o.cid = c.id "
        "GROUP BY c.region HAVING SUM(o.amt) > 30 ORDER BY c.region"
    )
    # kept: e = 65, w = 40 → ranked asc w=1, e=2.
    assert res.rows == [("e", 65, 2), ("w", 40, 1)]


def test_avg_window_over_join_group(storage):
    res = storage.q(
        "SELECT c.region, SUM(o.amt) AS s, AVG(SUM(o.amt)) OVER () AS avg_s "
        "FROM orders o JOIN customers c ON o.cid = c.id GROUP BY c.region ORDER BY c.region"
    )
    # mean of the group sums (65, 40) = 52.5.
    assert res.rows == [("e", 65, 52.5), ("w", 40, 52.5)]
