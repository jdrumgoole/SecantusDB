"""Aggregate ``FILTER (WHERE cond)`` — scope an aggregate to matching rows.

``agg(...) FILTER (WHERE cond)`` contributes only rows satisfying ``cond`` to the
aggregate. Lowered to a ``$cond`` inside each accumulator (neutral element 0 for
sum/count, NULL for avg/min/max). The ``$push``-based aggregates
(``array_agg`` / ``string_agg`` / ``jsonb_object_agg``) push a ``None`` sentinel
for non-matching rows and drop it in the post-``$group`` projection. Works in the
SELECT list (grouped, joined, and no-GROUP-BY), and in HAVING — and combines with
``DISTINCT`` (``count(DISTINCT x) FILTER (WHERE cond)``: the aggregate over the
distinct set of x-values drawn only from matching rows). Not supported with an
in-aggregate ORDER BY.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
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


def sqlstate(storage, session, sql):
    with pytest.raises(errors.SQLError) as ei:
        run(storage, session, sql)
    return ei.value.sqlstate


@pytest.fixture
def emp(storage, session):
    run(
        storage,
        session,
        "CREATE TABLE emp (id int PRIMARY KEY, dept text, sal int, age int, active bool)",
    )
    run(
        storage,
        session,
        "INSERT INTO emp VALUES (1,'a',100,30,true),(2,'a',200,50,false),"
        "(3,'a',300,45,true),(4,'b',400,60,true),(5,'b',500,25,false)",
    )
    return storage


# -- grouped ------------------------------------------------------------------ #


def test_count_filter_grouped(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, count(*) FILTER (WHERE active) FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 2), ("b", 1)]


def test_sum_filter_grouped(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, sum(sal) FILTER (WHERE age > 40) FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 500), ("b", 400)]


def test_avg_filter_grouped(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, avg(sal) FILTER (WHERE active) FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 200.0), ("b", 400.0)]


def test_min_max_filter_grouped(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, min(sal) FILTER (WHERE active), max(sal) FILTER (WHERE NOT active) "
        "FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 100, 200), ("b", 400, 500)]


def test_mixed_filtered_and_plain(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, count(*), count(*) FILTER (WHERE active) "
        "FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 3, 2), ("b", 2, 1)]


def test_filter_with_and_condition(emp, session):
    assert run(
        emp, session, "SELECT count(*) FILTER (WHERE active AND age > 40) FROM emp"
    ).rows == [(2,)]  # id 3 (45) and id 4 (60)


def test_filter_with_or_condition(emp, session):
    assert run(
        emp, session, "SELECT count(*) FILTER (WHERE age < 30 OR age > 55) FROM emp"
    ).rows == [(2,)]  # id 5 (25) and id 4 (60)


# -- no GROUP BY -------------------------------------------------------------- #


def test_count_star_filter_no_group(emp, session):
    assert run(emp, session, "SELECT count(*) FILTER (WHERE sal >= 300) FROM emp").rows == [(3,)]


def test_count_col_filter_no_group(emp, session):
    # count(col) FILTER counts non-null col among matching rows.
    assert run(emp, session, "SELECT count(sal) FILTER (WHERE active) FROM emp").rows == [(3,)]


def test_all_filtered_out_is_null_or_zero(emp, session):
    # count over an empty filtered set is 0; sum/max over one is NULL.
    rows = run(
        emp,
        session,
        "SELECT count(*) FILTER (WHERE age > 100), max(sal) FILTER (WHERE age > 100) FROM emp",
    ).rows
    assert rows == [(0, None)]


# -- HAVING ------------------------------------------------------------------- #


def test_having_with_filter(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept FROM emp GROUP BY dept HAVING count(*) FILTER (WHERE NOT active) >= 1 "
        "ORDER BY dept",
    ).rows
    assert rows == [("a",), ("b",)]  # both depts have an inactive row


def test_having_with_filter_excludes(storage, session):
    run(storage, session, "CREATE TABLE t (id int PRIMARY KEY, g text, flag bool)")
    run(storage, session, "INSERT INTO t VALUES (1,'a',true),(2,'a',false),(3,'b',false)")
    rows = run(
        storage,
        session,
        "SELECT g FROM t GROUP BY g HAVING count(*) FILTER (WHERE flag) >= 1 ORDER BY g",
    ).rows
    assert rows == [("a",)]  # only 'a' has a true flag


# -- JOIN + GROUP BY ---------------------------------------------------------- #


@pytest.fixture
def joined(storage, session):
    run(storage, session, "CREATE TABLE d (id int PRIMARY KEY, name text)")
    run(storage, session, "INSERT INTO d VALUES (1,'x'),(2,'y')")
    run(storage, session, "CREATE TABLE e (id int PRIMARY KEY, d_id int, sal int, active bool)")
    run(storage, session, "INSERT INTO e VALUES (10,1,100,true),(11,1,200,false),(12,2,300,true)")
    return storage


def test_filter_over_join_group(joined, session):
    rows = run(
        joined,
        session,
        "SELECT d.name, count(*) FILTER (WHERE e.active), sum(e.sal) FILTER (WHERE e.active) "
        "FROM d JOIN e ON e.d_id = d.id GROUP BY d.name ORDER BY d.name",
    ).rows
    assert rows == [("x", 1, 100), ("y", 1, 300)]


def test_having_filter_over_join(joined, session):
    rows = run(
        joined,
        session,
        "SELECT d.name FROM d JOIN e ON e.d_id = d.id GROUP BY d.name "
        "HAVING sum(e.sal) FILTER (WHERE e.active) >= 300 ORDER BY d.name",
    ).rows
    assert rows == [("y",)]  # x filtered-sum=100, y=300


# -- FILTER on the $push aggregates ------------------------------------------ #


def test_filter_on_array_agg(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, array_agg(sal) FILTER (WHERE active) FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", [100, 300]), ("b", [400])]


def test_filter_on_array_agg_keeps_matching_nulls(emp, session):
    # Postgres array_agg keeps NULL values of *matching* rows; only non-matching
    # rows are dropped.
    run(emp, session, "INSERT INTO emp VALUES (6,'a',NULL,70,true)")
    rows = run(
        emp,
        session,
        "SELECT array_agg(sal) FILTER (WHERE active) FROM emp WHERE dept='a'",
    ).rows
    assert rows == [([100, 300, None],)]


def test_filter_on_string_agg(emp, session):
    # active rows are a(1), a(3), b(4) → depts 'a','a','b'.
    rows = run(emp, session, "SELECT string_agg(dept, ',') FILTER (WHERE active) FROM emp").rows
    assert rows == [("a,a,b",)]


def test_filter_on_jsonb_object_agg(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, jsonb_object_agg(id, sal) FILTER (WHERE age > 40) "
        "FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", {"2": 200, "3": 300}), ("b", {"4": 400})]


def test_filter_on_array_agg_over_join(emp, session):
    run(emp, session, "CREATE TABLE dept (name text PRIMARY KEY, region text)")
    run(emp, session, "INSERT INTO dept VALUES ('a','west'), ('b','east')")
    rows = run(
        emp,
        session,
        "SELECT d.region, array_agg(e.sal) FILTER (WHERE e.active) "
        "FROM emp e JOIN dept d ON e.dept = d.name GROUP BY d.region ORDER BY d.region",
    ).rows
    assert rows == [("east", [400]), ("west", [100, 300])]


# -- unsupported -------------------------------------------------------------- #


def test_filter_on_array_agg_with_order_by_unsupported(emp, session):
    # FILTER combined with an in-aggregate ORDER BY isn't supported yet.
    assert (
        sqlstate(emp, session, "SELECT array_agg(sal ORDER BY sal) FILTER (WHERE active) FROM emp")
        == "0A000"
    )


# -- FILTER + DISTINCT -------------------------------------------------------- #


def test_count_distinct_filter_grouped(emp, session):
    # active rows: a{100,300}, b{400} -> distinct counts 2, 1.
    rows = run(
        emp,
        session,
        "SELECT dept, count(DISTINCT sal) FILTER (WHERE active) "
        "FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 2), ("b", 1)]


def test_sum_distinct_filter_grouped(emp, session):
    # a: 100+300=400 ; b: 400.
    rows = run(
        emp,
        session,
        "SELECT dept, sum(DISTINCT sal) FILTER (WHERE active) FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 400), ("b", 400)]


def test_avg_distinct_filter_grouped(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept, avg(DISTINCT sal) FILTER (WHERE active) FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 200.0), ("b", 400.0)]


def test_count_distinct_filter_dedups_matching_rows(emp, session):
    # A second active row with sal=100 in dept a must NOT change the distinct count.
    run(emp, session, "INSERT INTO emp VALUES (6,'a',100,33,true)")
    rows = run(
        emp,
        session,
        "SELECT dept, count(DISTINCT sal) FILTER (WHERE active) "
        "FROM emp GROUP BY dept ORDER BY dept",
    ).rows
    assert rows == [("a", 2), ("b", 1)]


def test_distinct_filter_in_having(emp, session):
    rows = run(
        emp,
        session,
        "SELECT dept FROM emp GROUP BY dept "
        "HAVING count(DISTINCT sal) FILTER (WHERE active) > 1 ORDER BY dept",
    ).rows
    assert rows == [("a",)]  # a has 2 distinct active sals, b has 1


def test_distinct_filter_over_join(emp, session):
    run(emp, session, "CREATE TABLE dept (name text PRIMARY KEY, region text)")
    run(emp, session, "INSERT INTO dept VALUES ('a','west'), ('b','east')")
    rows = run(
        emp,
        session,
        "SELECT d.region, count(DISTINCT e.sal) FILTER (WHERE e.active) "
        "FROM emp e JOIN dept d ON e.dept = d.name GROUP BY d.region ORDER BY d.region",
    ).rows
    assert rows == [("east", 1), ("west", 2)]
