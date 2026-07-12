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
from secantus.storage import Storage

DB = "testdb"


@pytest.fixture
def session():
    return Session(database=DB)


@pytest.fixture
def storage(session, tmp_path):
    s = Storage(str(tmp_path))

    def q(sql):
        run_sql(s, DB, sql, session=session)

    q("CREATE TABLE t (id bigint primary key, region text, city text, amt int)")
    for i, (r, c, a) in enumerate(
        [("e", "ny", 10), ("e", "ny", 20), ("e", "bos", 5), ("w", "sf", 30), ("w", "sf", 15)], 1
    ):
        q(f"INSERT INTO t (id, region, city, amt) VALUES ({i}, '{r}', '{c}', {a})")
    try:
        yield s
    finally:
        s.close()


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


# -- GROUPING SETS over a JOIN ----------------------------------------------- #


@pytest.fixture
def dim(storage, session):
    # A region → label dimension to join against `t`.
    run_sql(storage, DB, "CREATE TABLE u (region text primary key, label text)", session=session)
    run_sql(storage, DB, "INSERT INTO u VALUES ('e', 'East'), ('w', 'West')", session=session)
    return storage


def test_rollup_over_join(dim, session):
    got = rows(
        dim,
        session,
        "SELECT u.label, SUM(t.amt) FROM t JOIN u ON t.region = u.region "
        "GROUP BY ROLLUP(u.label) ORDER BY u.label NULLS LAST",
    )
    # East = 10+20+5 = 35, West = 30+15 = 45, grand total 80.
    assert got == [("East", 35), ("West", 45), (None, 80)]


def test_cube_over_join(dim, session):
    got = rows(
        dim,
        session,
        "SELECT u.label, t.city, SUM(t.amt) FROM t JOIN u ON t.region = u.region "
        "GROUP BY CUBE(u.label, t.city)",
    )
    # CUBE(label, city): (label,city)=3 + (label)=2 + (city)=3 + ()=1 = 9 rows.
    assert len(got) == 9
    assert (None, None, 80) in got  # grand total
    assert ("East", None, 35) in got  # label subtotal
    assert (None, "ny", 30) in got  # city subtotal across regions
    assert ("East", "ny", 30) in got  # leaf


def test_grouping_sets_over_join_count_distinct(dim, session):
    got = rows(
        dim,
        session,
        "SELECT u.label, count(DISTINCT t.amt) FROM t JOIN u ON t.region = u.region "
        "GROUP BY ROLLUP(u.label) ORDER BY u.label NULLS LAST",
    )
    # East distinct amts {10,20,5}=3, West {30,15}=2, all {10,20,5,30,15}=5.
    assert got == [("East", 3), ("West", 2), (None, 5)]


def test_grouping_sets_over_join_having(dim, session):
    got = rows(
        dim,
        session,
        "SELECT u.label, SUM(t.amt) FROM t JOIN u ON t.region = u.region "
        "GROUP BY ROLLUP(u.label) HAVING SUM(t.amt) >= 45 ORDER BY u.label NULLS LAST",
    )
    # East (35) filtered out; West (45) and grand total (80) survive.
    assert got == [("West", 45), (None, 80)]


def test_grouping_over_join_bitmask(dim, session):
    got = rows(
        dim,
        session,
        "SELECT u.label, GROUPING(u.label) AS g, SUM(t.amt) "
        "FROM t JOIN u ON t.region = u.region "
        "GROUP BY ROLLUP(u.label) ORDER BY u.label NULLS LAST",
    )
    # GROUPING(label) is 0 for the per-label rows, 1 for the rolled-up total.
    assert got == [("East", 0, 35), ("West", 0, 45), (None, 1, 80)]


def test_grouping_sets_over_join_computed_key(dim, session):
    # A computed grouping key over a JOIN (b222): lower each through the join
    # resolver into a synthetic field materialised on the join prefix.
    got = rows(
        dim,
        session,
        "SELECT lower(u.label) AS l, SUM(t.amt) FROM t JOIN u ON t.region = u.region "
        "GROUP BY ROLLUP(lower(u.label)) ORDER BY l NULLS LAST",
    )
    assert got == [("east", 35), ("west", 45), (None, 80)]


def test_grouping_sets_over_join_computed_key_having_grouping(dim, session):
    # Computed key + HAVING + GROUPING() over a join.
    got = rows(
        dim,
        session,
        "SELECT lower(u.label) AS l, GROUPING(lower(u.label)) AS g, SUM(t.amt) AS tot "
        "FROM t JOIN u ON t.region = u.region GROUP BY ROLLUP(lower(u.label)) "
        "HAVING SUM(t.amt) >= 45 ORDER BY l NULLS LAST",
    )
    # East (35) filtered out; West (45) and grand total (80, GROUPING=1) survive.
    assert got == [("west", 0, 45), (None, 1, 80)]


def test_grouping_sets_over_join_computed_key_window(dim, session):
    # Computed key under a window over GROUPING SETS + JOIN (b221 + b222 paths).
    got = rows(
        dim,
        session,
        "SELECT lower(u.label) AS l, SUM(t.amt) AS tot, "
        "row_number() OVER (ORDER BY SUM(t.amt) DESC) AS rn "
        "FROM t JOIN u ON t.region = u.region GROUP BY ROLLUP(lower(u.label)) ORDER BY rn",
    )
    assert got == [(None, 80, 1), ("west", 45, 2), ("east", 35, 3)]


def test_grouping_sets_over_join_unlowerable_key_rejected(dim, session):
    # A key using a function the aggregation engine can't lower still raises 0A000.
    with pytest.raises(errors.SQLError) as ei:
        rows(
            dim,
            session,
            "SELECT substr(u.label, 1, 1), SUM(t.amt) FROM t JOIN u ON t.region = u.region "
            "GROUP BY ROLLUP(substr(u.label, 1, 1))",
        )
    assert ei.value.sqlstate == "0A000"


def test_window_over_grouping_sets_join(dim, session):
    # A window over GROUPING SETS that also sits over a JOIN (b221).
    got = rows(
        dim,
        session,
        "SELECT u.label, SUM(t.amt) AS tot, row_number() OVER (ORDER BY SUM(t.amt)) AS rn "
        "FROM t JOIN u ON t.region = u.region GROUP BY ROLLUP(u.label) ORDER BY rn",
    )
    # East = 35, West = 45, grand total = 80 → ranked ascending by sum.
    assert got == [("East", 35, 1), ("West", 45, 2), (None, 80, 3)]


def test_window_over_grouping_sets_join_grouping_helper(dim, session):
    # GROUPING() usable inside the window ORDER BY over the join.
    got = rows(
        dim,
        session,
        "SELECT u.label, GROUPING(u.label) AS g, SUM(t.amt) AS tot, "
        "row_number() OVER (ORDER BY GROUPING(u.label), u.label) AS rn "
        "FROM t JOIN u ON t.region = u.region GROUP BY ROLLUP(u.label) ORDER BY rn",
    )
    assert got == [("East", 0, 35, 1), ("West", 0, 45, 2), (None, 1, 80, 3)]


def test_window_over_grouping_sets_join_having(dim, session):
    # HAVING prunes grouped rows before the window ranks the survivors.
    got = rows(
        dim,
        session,
        "SELECT u.label, SUM(t.amt) AS tot, row_number() OVER (ORDER BY SUM(t.amt)) AS rn "
        "FROM t JOIN u ON t.region = u.region GROUP BY ROLLUP(u.label) "
        "HAVING SUM(t.amt) >= 45 ORDER BY rn",
    )
    # East (35) filtered out; West (45) and grand total (80) survive.
    assert got == [("West", 45, 1), (None, 80, 2)]


def test_window_over_grouping_sets_join_count_distinct(dim, session):
    got = rows(
        dim,
        session,
        "SELECT u.label, count(DISTINCT t.amt) AS c, "
        "rank() OVER (ORDER BY count(DISTINCT t.amt) DESC) AS rk "
        "FROM t JOIN u ON t.region = u.region GROUP BY ROLLUP(u.label) "
        "ORDER BY rk, u.label NULLS LAST",
    )
    # East {10,20,5}=3, West {30,15}=2, all {10,20,5,30,15}=5 → rank by distinct desc.
    assert got == [(None, 5, 1), ("East", 3, 2), ("West", 2, 3)]


# -- DISTINCT aggregates under GROUPING SETS --------------------------------- #


def _order(got):
    return sorted(got, key=lambda r: (r[0] is None, r[0]))


def test_rollup_count_distinct(storage, session):
    # e amts {10,20,5}=3 distinct, w {30,15}=2; grand total {10,20,5,30,15}=5.
    got = rows(
        storage,
        session,
        "SELECT region, count(DISTINCT amt) AS d FROM t GROUP BY ROLLUP(region)",
    )
    assert _order(got) == [("e", 3), ("w", 2), (None, 5)]


def test_rollup_sum_distinct(storage, session):
    # e distinct {10,20,5}=35, w {30,15}=45, total {10,20,5,30,15}=80.
    got = rows(
        storage,
        session,
        "SELECT region, sum(DISTINCT amt) AS s FROM t GROUP BY ROLLUP(region)",
    )
    assert _order(got) == [("e", 35), ("w", 45), (None, 80)]


def test_cube_distinct_and_plain_together(storage, session):
    # DISTINCT and a plain aggregate coexist in one CUBE query.
    got = rows(
        storage,
        session,
        "SELECT region, count(DISTINCT amt) AS d, count(*) AS c FROM t GROUP BY CUBE(region)",
    )
    assert _order(got) == [("e", 3, 3), ("w", 2, 2), (None, 5, 5)]


def test_grouping_sets_min_distinct(storage, session):
    # min(DISTINCT) == min; works under GROUPING SETS via the plain accumulator.
    got = rows(
        storage,
        session,
        "SELECT region, min(DISTINCT amt) AS m FROM t GROUP BY GROUPING SETS ((region), ())",
    )
    assert _order(got) == [("e", 5), ("w", 15), (None, 5)]


def test_rollup_count_distinct_filter(storage, session):
    # FILTER + DISTINCT under ROLLUP: only amt > 10 contributes.
    got = rows(
        storage,
        session,
        "SELECT region, count(DISTINCT amt) FILTER (WHERE amt > 10) AS d "
        "FROM t GROUP BY ROLLUP(region)",
    )
    # e: {20}=1 ; w: {30,15}=2 ; total: {20,30,15}=3.
    assert _order(got) == [("e", 1), ("w", 2), (None, 3)]


def test_rollup_count_distinct_star_rejected(storage, session):
    with pytest.raises(errors.SQLError) as ei:
        rows(storage, session, "SELECT region, count(DISTINCT *) FROM t GROUP BY ROLLUP(region)")
    assert ei.value.sqlstate == "0A000"


# -- HAVING with GROUPING SETS ----------------------------------------------- #


def test_rollup_having_on_count(storage, session):
    # region e has 3 rows, w has 2; the grand-total row has 5. HAVING count(*) > 2
    # keeps e and the grand total.
    got = rows(
        storage,
        session,
        "SELECT region, count(*) AS c FROM t GROUP BY ROLLUP(region) HAVING count(*) > 2",
    )
    assert _order(got) == [("e", 3), (None, 5)]


def test_rollup_having_on_sum(storage, session):
    # e sum 35, w sum 45, grand total 80. HAVING sum(amt) >= 45 keeps w + total.
    got = rows(
        storage,
        session,
        "SELECT region, sum(amt) AS s FROM t GROUP BY ROLLUP(region) HAVING sum(amt) >= 45",
    )
    assert _order(got) == [("w", 45), (None, 80)]


def test_grouping_sets_having_on_group_column(storage, session):
    # HAVING on a group column: only the region='e' subtotal row (the grand-total
    # row has region NULL and is excluded).
    got = rows(
        storage,
        session,
        "SELECT region, count(*) AS c FROM t GROUP BY ROLLUP(region) HAVING region = 'e'",
    )
    assert got == [("e", 3)]


def test_rollup_having_aggregate_not_selected(storage, session):
    # HAVING references an aggregate that isn't in the select list.
    got = rows(
        storage,
        session,
        "SELECT region FROM t GROUP BY ROLLUP(region) HAVING sum(amt) > 40",
    )
    assert _order(got) == [("w",), (None,)]  # w=45, total=80; e=35 excluded


def test_cube_having_and_predicate(storage, session):
    got = rows(
        storage,
        session,
        "SELECT region, count(*) AS c FROM t GROUP BY CUBE(region) "
        "HAVING count(*) >= 2 AND sum(amt) > 40",
    )
    assert _order(got) == [("w", 2), (None, 5)]


def test_rollup_having_count_distinct(storage, session):
    # count(DISTINCT city) per region: e has {ny, bos}=2, w has {sf}=1; grand total
    # {ny, bos, sf}=3. HAVING count(DISTINCT city) >= 2 keeps e + grand total.
    got = rows(
        storage,
        session,
        "SELECT region, count(DISTINCT city) AS d FROM t GROUP BY ROLLUP(region) "
        "HAVING count(DISTINCT city) >= 2",
    )
    assert _order(got) == [("e", 2), (None, 3)]


# -- statistical / bitwise aggregates under GROUPING SETS (b223) -------------- #


def test_variance_under_rollup(storage, session):
    # Sample variance per region + grand total. amt: e=[10,20,5], w=[30,15].
    got = _order(
        rows(storage, session, "SELECT region, variance(amt) AS v FROM t GROUP BY ROLLUP(region)")
    )
    assert [r[0] for r in got] == ["e", "w", None]
    assert got[0][1] == pytest.approx(58.33333333, rel=1e-6)  # var([10,20,5])
    assert got[1][1] == pytest.approx(112.5)  # var([30,15])
    assert got[2][1] == pytest.approx(92.5)  # var([10,20,5,30,15])


def test_var_pop_and_stddev_under_cube(storage, session):
    got = _order(
        rows(
            storage,
            session,
            "SELECT region, var_pop(amt) AS v, stddev(amt) AS s FROM t GROUP BY CUBE(region)",
        )
    )
    assert got[0][1] == pytest.approx(38.88888889, rel=1e-6)  # pvar([10,20,5])
    assert got[2][1] == pytest.approx(74.0)  # pvar of all five
    assert got[0][2] == pytest.approx(7.63762616, rel=1e-6)  # stddev([10,20,5])


def test_bit_aggregates_under_rollup(storage, session):
    session2 = session
    rows(storage, session2, "CREATE TABLE b (id int primary key, g text, n int)")
    rows(storage, session2, "INSERT INTO b VALUES (1,'x',6),(2,'x',3),(3,'y',12),(4,'y',10)")
    got = _order(
        rows(
            storage,
            session2,
            "SELECT g, bit_and(n) AS a, bit_or(n) AS o, bit_xor(n) AS x FROM b GROUP BY ROLLUP(g)",
        )
    )
    # x: 6&3=2, 6|3=7, 6^3=5 ; y: 12&10=8, 12|10=14, 12^10=6 ; all: &=0, |=15, ^=3.
    assert got == [("x", 2, 7, 5), ("y", 8, 14, 6), (None, 0, 15, 3)]


def test_variance_and_plain_agg_together_under_rollup(storage, session):
    got = _order(
        rows(
            storage,
            session,
            "SELECT region, SUM(amt) AS s, variance(amt) AS v FROM t GROUP BY ROLLUP(region)",
        )
    )
    # _order sorts region ascending, NULL last: e, w, grand total.
    assert (
        got[0][0] == "e" and got[0][1] == 35 and got[0][2] == pytest.approx(58.33333333, rel=1e-6)
    )
    assert got[1][0] == "w" and got[1][1] == 45 and got[1][2] == pytest.approx(112.5)
    assert got[2][0] is None and got[2][1] == 80 and got[2][2] == pytest.approx(92.5)


def test_variance_over_grouping_sets_join(dim, session):
    got = rows(
        dim,
        session,
        "SELECT u.label, variance(t.amt) AS v FROM t JOIN u ON t.region = u.region "
        "GROUP BY ROLLUP(u.label) ORDER BY u.label NULLS LAST",
    )
    assert got[0][0] == "East" and got[0][1] == pytest.approx(58.33333333, rel=1e-6)
    assert got[1][0] == "West" and got[1][1] == pytest.approx(112.5)
    assert got[2][0] is None and got[2][1] == pytest.approx(92.5)


def test_bit_xor_over_grouping_sets_join(storage, session):
    rows(storage, session, "CREATE TABLE b (id int primary key, g text, n int)")
    rows(storage, session, "INSERT INTO b VALUES (1,'x',6),(2,'x',3),(3,'y',12),(4,'y',10)")
    rows(storage, session, "CREATE TABLE bd (g text primary key, lbl text)")
    rows(storage, session, "INSERT INTO bd VALUES ('x','X'),('y','Y')")
    got = rows(
        storage,
        session,
        "SELECT bd.lbl, bit_xor(b.n) AS x FROM b JOIN bd ON b.g = bd.g "
        "GROUP BY ROLLUP(bd.lbl) ORDER BY bd.lbl NULLS LAST",
    )
    assert got == [("X", 5), ("Y", 6), (None, 3)]


# -- in-aggregate ORDER BY (sorted array_agg / string_agg) under GROUPING SETS (b224) -- #


def test_array_agg_ordered_under_rollup(storage, session):
    # array_agg(amt ORDER BY amt) per grouping set; amt: e=[10,20,5], w=[30,15].
    got = _order(
        rows(
            storage,
            session,
            "SELECT region, array_agg(amt ORDER BY amt) AS a FROM t GROUP BY ROLLUP(region)",
        )
    )
    assert got == [("e", [5, 10, 20]), ("w", [15, 30]), (None, [5, 10, 15, 20, 30])]


def test_array_agg_ordered_desc_under_cube(storage, session):
    got = _order(
        rows(
            storage,
            session,
            "SELECT region, array_agg(amt ORDER BY amt DESC) AS a FROM t GROUP BY CUBE(region)",
        )
    )
    assert got == [("e", [20, 10, 5]), ("w", [30, 15]), (None, [30, 20, 15, 10, 5])]


def test_string_agg_ordered_under_rollup(storage, session):
    # cities per region: e={ny,ny,bos}, w={sf,sf}. ORDER BY city then dedup-free join.
    got = _order(
        rows(
            storage,
            session,
            "SELECT region, string_agg(city, ',' ORDER BY city) AS s "
            "FROM t GROUP BY ROLLUP(region)",
        )
    )
    assert got == [
        ("e", "bos,ny,ny"),
        ("w", "sf,sf"),
        (None, "bos,ny,ny,sf,sf"),
    ]


def test_array_agg_ordered_with_plain_agg_under_rollup(storage, session):
    got = _order(
        rows(
            storage,
            session,
            "SELECT region, SUM(amt) AS s, array_agg(amt ORDER BY amt) AS a "
            "FROM t GROUP BY ROLLUP(region)",
        )
    )
    assert got == [
        ("e", 35, [5, 10, 20]),
        ("w", 45, [15, 30]),
        (None, 80, [5, 10, 15, 20, 30]),
    ]


def test_array_agg_ordered_over_grouping_sets_join(dim, session):
    got = rows(
        dim,
        session,
        "SELECT u.label, array_agg(t.amt ORDER BY t.amt) AS a FROM t JOIN u ON t.region = u.region "
        "GROUP BY ROLLUP(u.label) ORDER BY u.label NULLS LAST",
    )
    assert got == [("East", [5, 10, 20]), ("West", [15, 30]), (None, [5, 10, 15, 20, 30])]


def test_string_agg_ordered_desc_over_grouping_sets_join(dim, session):
    got = rows(
        dim,
        session,
        "SELECT u.label, string_agg(city, ',' ORDER BY city DESC) AS s "
        "FROM t JOIN u ON t.region = u.region GROUP BY ROLLUP(u.label) ORDER BY u.label NULLS LAST",
    )
    assert got == [("East", "ny,ny,bos"), ("West", "sf,sf"), (None, "sf,sf,ny,ny,bos")]


def test_array_agg_ordered_filter_under_rollup_rejected(storage, session):
    # FILTER combined with an in-aggregate ORDER BY is still 0A000.
    with pytest.raises(errors.SQLError) as ei:
        rows(
            storage,
            session,
            "SELECT region, array_agg(amt ORDER BY amt) FILTER (WHERE amt > 8) "
            "FROM t GROUP BY ROLLUP(region)",
        )
    assert ei.value.sqlstate == "0A000"
