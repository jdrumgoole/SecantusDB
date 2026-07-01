"""Window functions computed over GROUP BY aggregates in one SELECT.

Postgres evaluates window functions *after* grouping/aggregation, so a window's
args / PARTITION BY / ORDER BY may reference the group aggregates
(``RANK() OVER (ORDER BY SUM(sal))``) and an aggregate may nest inside a window
aggregate (``SUM(SUM(sal)) OVER ()``). The planner runs the ``$group`` first,
then computes the windows over the grouped rows.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def storage():
    s = FakeStorage()
    s.q = lambda sql: run_sql(s, DB, sql, session=Session(database=DB))[0]
    s.q("CREATE TABLE emp (id bigint primary key, dept text, region text, sal int)")
    rows = [
        (1, "a", "e", 10),
        (2, "a", "e", 20),
        (3, "b", "e", 30),
        (4, "b", "w", 5),
        (5, "b", "w", 15),
        (6, "c", "w", 40),
    ]
    for i, d, rg, sa in rows:
        s.q(f"INSERT INTO emp (id, dept, region, sal) VALUES ({i}, '{d}', '{rg}', {sa})")
    return s


def test_rank_over_group_aggregate(storage):
    # SUM(sal) per dept: a=30, b=50, c=40 → ranked ascending 1, 3, 2.
    res = storage.q(
        "SELECT dept, SUM(sal) AS s, RANK() OVER (ORDER BY SUM(sal)) AS rk "
        "FROM emp GROUP BY dept ORDER BY dept"
    )
    assert res.rows == [("a", 30, 1), ("b", 50, 3), ("c", 40, 2)]
    assert [c.type_tag for c in res.columns] == ["text", "int4", "int8"]


def test_aggregate_nested_in_window_aggregate(storage):
    # SUM(SUM(sal)) OVER () — grand total (30+50+40 = 120) broadcast to every row.
    res = storage.q(
        "SELECT dept, SUM(sal) AS s, SUM(SUM(sal)) OVER () AS total "
        "FROM emp GROUP BY dept ORDER BY dept"
    )
    assert res.rows == [("a", 30, 120), ("b", 50, 120), ("c", 40, 120)]


def test_row_number_over_count(storage):
    # COUNT(*) per dept: a=2, b=3, c=1 → row_number by count desc: b, a, c.
    res = storage.q(
        "SELECT dept, COUNT(*) AS c, ROW_NUMBER() OVER (ORDER BY COUNT(*) DESC) AS rn "
        "FROM emp GROUP BY dept ORDER BY rn"
    )
    assert res.rows == [("b", 3, 1), ("a", 2, 2), ("c", 1, 3)]


def test_partition_by_group_column(storage):
    # RANK within each region over the per-(region,dept) sums, highest first.
    res = storage.q(
        "SELECT region, dept, SUM(sal) AS s, "
        "RANK() OVER (PARTITION BY region ORDER BY SUM(sal) DESC) AS rk "
        "FROM emp GROUP BY region, dept ORDER BY region, rk"
    )
    # region e: (a=30, b=30) tie → both rank 1; region w: c=40 rank1, b=20 rank2.
    assert res.rows == [
        ("e", "a", 30, 1),
        ("e", "b", 30, 1),
        ("w", "c", 40, 1),
        ("w", "b", 20, 2),
    ]


def test_implicit_whole_table_group(storage):
    # No GROUP BY: the aggregate implies a single group, the window sees one row.
    res = storage.q("SELECT SUM(sal) AS s, RANK() OVER (ORDER BY SUM(sal)) AS rk FROM emp")
    assert res.rows == [(120, 1)]


def test_having_with_window(storage):
    # HAVING prunes groups before the window ranks the survivors.
    res = storage.q(
        "SELECT dept, SUM(sal) AS s, RANK() OVER (ORDER BY SUM(sal)) AS rk "
        "FROM emp GROUP BY dept HAVING SUM(sal) > 35 ORDER BY dept"
    )
    # kept: b=50, c=40 → ranked 2, 1.
    assert res.rows == [("b", 50, 2), ("c", 40, 1)]


def test_order_by_window_alias(storage):
    res = storage.q(
        "SELECT dept, SUM(sal) AS s, RANK() OVER (ORDER BY SUM(sal)) AS rk "
        "FROM emp GROUP BY dept ORDER BY rk DESC"
    )
    assert res.rows == [("b", 50, 3), ("c", 40, 2), ("a", 30, 1)]


def test_avg_window_over_group_sum(storage):
    # AVG(SUM(sal)) OVER () — mean of the group sums (30, 50, 40) = 40.
    res = storage.q(
        "SELECT dept, SUM(sal) AS s, AVG(SUM(sal)) OVER () AS avg_s "
        "FROM emp GROUP BY dept ORDER BY dept"
    )
    assert res.rows == [("a", 30, 40.0), ("b", 50, 40.0), ("c", 40, 40.0)]


def test_non_grouped_column_rejected(storage):
    with pytest.raises(errors.SQLError) as ei:
        storage.q("SELECT dept, sal, RANK() OVER (ORDER BY SUM(sal)) FROM emp GROUP BY dept")
    assert ei.value.sqlstate == "42803"
