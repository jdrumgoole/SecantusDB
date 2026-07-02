"""``GROUP BY ROLLUP / CUBE / GROUPING SETS`` — multi-grouping aggregation.

Each is the UNION of a plain GROUP BY over several *grouping sets*; a group
column absent from a given set reads NULL in that set's rows. We enumerate the
sets (ROLLUP → prefixes, CUBE → all subsets, GROUPING SETS → as written; a
leading plain `GROUP BY a, …` is a prefix in every set) and combine per-set
``$group`` branches with ``$unionWith``.
"""

from __future__ import annotations

import pytest

from secantus.sql import errors, run_sql
from secantus.sql.session import Session
from sqlfake import FakeStorage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(session):
    s = FakeStorage()

    def q(sql):
        run_sql(s, DB, sql, session=session)

    q("CREATE TABLE t (id bigint primary key, region text, city text, amt int)")
    for i, (r, c, a) in enumerate(
        [("e", "ny", 10), ("e", "ny", 20), ("e", "bos", 5), ("w", "sf", 30), ("w", "sf", 15)], 1
    ):
        q(f"INSERT INTO t (id, region, city, amt) VALUES ({i}, '{r}', '{c}', {a})")
    return s


def rows(storage, session, sql):
    return run_sql(storage, DB, sql, session=session)[-1].rows


def test_rollup_adds_grand_total(storage, session):
    # ROLLUP(region) → per-region subtotals + a grand-total row (region NULL).
    got = rows(
        storage,
        session,
        "SELECT region, SUM(amt) AS s FROM t GROUP BY ROLLUP(region)",
    )
    assert sorted(got, key=lambda r: (r[0] is None, r[0])) == [("e", 35), ("w", 45), (None, 80)]


def test_rollup_two_levels(storage, session):
    # ROLLUP(region, city) → (region,city), (region), () — 3 + 2 + 1 rows.
    got = rows(
        storage,
        session,
        "SELECT region, city, SUM(amt) FROM t GROUP BY ROLLUP(region, city)",
    )
    assert (None, None, 80) in got  # grand total
    assert ("e", None, 35) in got  # region subtotal
    assert ("e", "ny", 30) in got  # leaf
    assert len(got) == 6


def test_grouping_sets_explicit(storage, session):
    got = rows(
        storage,
        session,
        "SELECT region, city, SUM(amt) FROM t GROUP BY GROUPING SETS ((region), (city), ())",
    )
    assert ("e", None, 35) in got and ("w", None, 45) in got
    assert (None, "ny", 30) in got and (None, "bos", 5) in got and (None, "sf", 45) in got
    assert (None, None, 80) in got
    assert len(got) == 6


def test_cube_all_subsets(storage, session):
    # CUBE(region, city) → (r,c) + (r) + (c) + () = 3 + 2 + 3 + 1 = 9.
    got = rows(
        storage,
        session,
        "SELECT region, city, SUM(amt) FROM t GROUP BY CUBE(region, city)",
    )
    assert len(got) == 9
    assert (None, None, 80) in got


def test_leading_col_is_prefix_in_every_set(storage, session):
    # GROUP BY region, ROLLUP(city) → region present in every set:
    # (region, city) and (region).
    got = rows(
        storage,
        session,
        "SELECT region, city, SUM(amt) FROM t GROUP BY region, ROLLUP(city)",
    )
    assert ("e", "ny", 30) in got and ("e", "bos", 5) in got
    assert ("e", None, 35) in got and ("w", None, 45) in got
    assert (None, None, 80) not in got  # region is never rolled up
    assert len(got) == 5


def test_grouping_sets_with_where_and_order(storage, session):
    got = rows(
        storage,
        session,
        "SELECT region, SUM(amt) AS s FROM t WHERE amt > 8 GROUP BY ROLLUP(region) ORDER BY s",
    )
    # amt>8 keeps e:(10,20)=30, w:(30,15)=45 → grand 75, ordered by sum asc.
    assert got == [("e", 30), ("w", 45), (None, 75)]


def test_grouping_sets_over_join_rejected(storage, session):
    run_sql(storage, DB, "CREATE TABLE u (id bigint primary key, region text)", session=session)
    with pytest.raises(errors.SQLError):
        rows(
            storage,
            session,
            "SELECT t.region, SUM(t.amt) FROM t JOIN u ON t.region = u.region "
            "GROUP BY ROLLUP(t.region)",
        )
