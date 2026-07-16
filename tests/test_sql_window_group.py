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
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def storage(tmp_path):
    s = Storage(str(tmp_path))
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
    try:
        yield s
    finally:
        s.close()


def test_rank_over_group_aggregate(storage):
    # SUM(sal) per dept: a=30, b=50, c=40 → ranked ascending 1, 3, 2.
    res = storage.q(
        "SELECT dept, SUM(sal) AS s, RANK() OVER (ORDER BY SUM(sal)) AS rk "
        "FROM emp GROUP BY dept ORDER BY dept"
    )
    assert res.rows == [("a", 30, 1), ("b", 50, 3), ("c", 40, 2)]
    # sum(int) is bigint in Postgres.
    assert [c.type_tag for c in res.columns] == ["text", "int8", "int8"]


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


# -- window over GROUPING SETS / ROLLUP / CUBE (b219) ------------------------- #


def test_row_number_over_rollup(storage):
    # SUM(sal) per region: e=60 (10+20+30), w=60 (5+15+40); grand total 120.
    # Ordered by SUM(sal) DESC then region: the grand-total row (120) ranks first.
    res = storage.q(
        "SELECT region, SUM(sal) AS s, "
        "ROW_NUMBER() OVER (ORDER BY SUM(sal) DESC, region NULLS FIRST) AS rn "
        "FROM emp GROUP BY ROLLUP(region) ORDER BY rn"
    )
    assert res.rows == [(None, 120, 1), ("e", 60, 2), ("w", 60, 3)]


def test_rank_partition_over_cube(storage):
    # CUBE(region) → per-region rows + the grand total. PARTITION BY region makes
    # each row its own partition, so every rank is 1.
    res = storage.q(
        "SELECT region, SUM(sal) AS s, "
        "RANK() OVER (PARTITION BY region ORDER BY SUM(sal)) AS rk "
        "FROM emp GROUP BY CUBE(region) ORDER BY region NULLS LAST"
    )
    assert res.rows == [("e", 60, 1), ("w", 60, 1), (None, 120, 1)]


def test_grouping_helper_with_window(storage):
    # GROUPING(region) is 0 on the per-region rows, 1 on the rolled-up total; the
    # window orders by (GROUPING, region) so the total sorts last.
    res = storage.q(
        "SELECT region, GROUPING(region) AS g, SUM(sal) AS s, "
        "ROW_NUMBER() OVER (ORDER BY GROUPING(region), region) AS rn "
        "FROM emp GROUP BY ROLLUP(region) ORDER BY rn"
    )
    assert res.rows == [("e", 0, 60, 1), ("w", 0, 60, 2), (None, 1, 120, 3)]


def test_having_and_window_over_rollup(storage):
    # HAVING prunes grouped rows before the window ranks the survivors.
    res = storage.q(
        "SELECT region, SUM(sal) AS s, ROW_NUMBER() OVER (ORDER BY region NULLS LAST) AS rn "
        "FROM emp GROUP BY ROLLUP(region) HAVING SUM(sal) >= 100 ORDER BY rn"
    )
    # only the grand total (120) survives HAVING SUM(sal) >= 100.
    assert res.rows == [(None, 120, 1)]


def test_running_window_over_grouping_sets(storage):
    # A running SUM over the grouped rows of GROUPING SETS ((region), ()).
    res = storage.q(
        "SELECT region, SUM(sal) AS s, "
        "SUM(SUM(sal)) OVER (ORDER BY region NULLS LAST) AS run "
        "FROM emp GROUP BY GROUPING SETS ((region), ()) ORDER BY region NULLS LAST"
    )
    # e=60 (run 60), w=60 (run 120), total=120 (run 240).
    assert res.rows == [("e", 60, 60), ("w", 60, 120), (None, 120, 240)]


def test_column_tags_window_over_rollup(storage):
    res = storage.q(
        "SELECT region, SUM(sal) AS s, ROW_NUMBER() OVER (ORDER BY SUM(sal)) AS rn "
        "FROM emp GROUP BY ROLLUP(region)"
    )
    # sum(int) is bigint in Postgres.
    assert [c.type_tag for c in res.columns] == ["text", "int8", "int8"]
