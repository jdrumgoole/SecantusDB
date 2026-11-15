"""Gap tests for an UPDATE ... FROM and a DELETE ... USING, which this server
rewrites into correlated subqueries over the extra FROM items
(`rewrite_dml_from`).

Each answer was measured on a private PostgreSQL 15.19 cluster with this
module's tables (found while working RP-432).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.test_rp_analysis_order import _answers


@pytest.mark.backlog_fixed("RP-435")
def test_explain_pulls_up_a_constant_subquery(rust_pg: Any) -> None:
    """EXPLAIN of an UPDATE ... FROM or a DELETE ... USING a one-row
    constant subquery pulled it up into an Index Cond on the target's key
    (`pull_up_subqueries`); it printed a Seq Scan of the target."""
    assert _answers(
        rust_pg,
        [
            "explain (costs off) delete from k using (select 50 as a) t where k.a = t.a",
            "explain (costs off) update k set b = 'x' from (select 50 as a) t where k.a = t.a",
        ],
    ) == [
        (
            "ok",
            [
                ("Delete on k",),
                ("  ->  Index Scan using k_pkey on k",),
                ("        Index Cond: (a = 50)",),
            ],
        ),
        (
            "ok",
            [
                ("Update on k",),
                ("  ->  Index Scan using k_pkey on k",),
                ("        Index Cond: (a = 50)",),
            ],
        ),
    ]


_ANALYSED = ["insert into k select * from e", "analyze e", "analyze k"]


@pytest.mark.backlog_fixed("RP-435")
def test_explain_plans_the_join(rust_pg: Any) -> None:
    """EXPLAIN of an UPDATE ... FROM or a DELETE ... USING a table planned
    the join PostgreSQL makes of it, under the ModifyTable; it printed a Seq
    Scan of the target alone."""
    got = _answers(
        rust_pg,
        _ANALYSED
        + [
            "explain (costs off) delete from k using e where k.a = e.a",
            "explain (costs off) update k x set b = e.b from e where x.a = e.a",
            "explain (costs off) delete from k using e where k.a = e.a and e.b = 'v3'",
            "explain delete from k using e where k.a = e.a",
        ],
    )[len(_ANALYSED) :]
    assert got[:3] == [
        (
            "ok",
            [
                ("Delete on k",),
                ("  ->  Hash Join",),
                ("        Hash Cond: (e.a = k.a)",),
                ("        ->  Seq Scan on e",),
                ("        ->  Hash",),
                ("              ->  Seq Scan on k",),
            ],
        ),
        (
            "ok",
            [
                ("Update on k x",),
                ("  ->  Hash Join",),
                ("        Hash Cond: (e.a = x.a)",),
                ("        ->  Seq Scan on e",),
                ("        ->  Hash",),
                ("              ->  Seq Scan on k x",),
            ],
        ),
        (
            "ok",
            [
                ("Delete on k",),
                ("  ->  Hash Join",),
                ("        Hash Cond: (k.a = e.a)",),
                ("        ->  Seq Scan on k",),
                ("        ->  Hash",),
                ("              ->  Seq Scan on e",),
                ("                    Filter: (b = 'v3'::text)",),
            ],
        ),
    ]
    # The ModifyTable costs what its join does and returns nothing.
    assert got[3][1][0] == ("Delete on k  (cost=1.23..2.36 rows=0 width=0)",)


@pytest.mark.backlog_fixed("RP-435")
def test_explain_join_widths(rust_pg: Any) -> None:
    """A DML join carried each relation's `ctid` and the columns the SET
    list reads, not the tables' every column: PostgreSQL 15.19's join is
    `width=12` for a DELETE, each scan `width=10` (`ctid` and `a`); with
    `set b = e.b` the join is 15 and `e`'s scan 13, with `set a = e.a` the
    join 16. It printed the widths of the tables' every column."""
    got = _answers(
        rust_pg,
        _ANALYSED
        + [
            "explain delete from k using e where k.a = e.a",
            "explain update k set b = e.b from e where k.a = e.a",
            "explain update k set a = e.a from e where k.a = e.a",
        ],
    )[len(_ANALYSED) :]
    assert got == [
        (
            "ok",
            [
                ("Delete on k  (cost=1.23..2.36 rows=0 width=0)",),
                ("  ->  Hash Join  (cost=1.23..2.36 rows=10 width=12)",),
                ("        Hash Cond: (e.a = k.a)",),
                ("        ->  Seq Scan on e  (cost=0.00..1.10 rows=10 width=10)",),
                ("        ->  Hash  (cost=1.10..1.10 rows=10 width=10)",),
                ("              ->  Seq Scan on k  (cost=0.00..1.10 rows=10 width=10)",),
            ],
        ),
        (
            "ok",
            [
                ("Update on k  (cost=1.23..2.36 rows=0 width=0)",),
                ("  ->  Hash Join  (cost=1.23..2.36 rows=10 width=15)",),
                ("        Hash Cond: (e.a = k.a)",),
                ("        ->  Seq Scan on e  (cost=0.00..1.10 rows=10 width=13)",),
                ("        ->  Hash  (cost=1.10..1.10 rows=10 width=10)",),
                ("              ->  Seq Scan on k  (cost=0.00..1.10 rows=10 width=10)",),
            ],
        ),
        (
            "ok",
            [
                ("Update on k  (cost=1.23..2.36 rows=0 width=0)",),
                ("  ->  Hash Join  (cost=1.23..2.36 rows=10 width=16)",),
                ("        Hash Cond: (e.a = k.a)",),
                ("        ->  Seq Scan on e  (cost=0.00..1.10 rows=10 width=10)",),
                ("        ->  Hash  (cost=1.10..1.10 rows=10 width=10)",),
                ("              ->  Seq Scan on k  (cost=0.00..1.10 rows=10 width=10)",),
            ],
        ),
    ]


@pytest.mark.backlog_fixed("RP-435")
def test_explain_join_width_of_a_computed_set_value(rust_pg: Any) -> None:
    """A SET value computed from a constant is a column of the join's target
    list, of its type's width: PostgreSQL 15.19's `set b = 'x'` join is
    `width=44` (two `ctid`s and a `text`), each scan `width=10`. The plan
    was unpriced, as any join whose select list computed an expression
    was."""
    got = _answers(
        rust_pg,
        _ANALYSED + ["explain update k set b = 'x' from e where k.a = e.a"],
    )[len(_ANALYSED) :]
    assert got == [
        (
            "ok",
            [
                ("Update on k  (cost=1.23..2.36 rows=0 width=0)",),
                ("  ->  Hash Join  (cost=1.23..2.36 rows=10 width=44)",),
                ("        Hash Cond: (e.a = k.a)",),
                ("        ->  Seq Scan on e  (cost=0.00..1.10 rows=10 width=10)",),
                ("        ->  Hash  (cost=1.10..1.10 rows=10 width=10)",),
                ("              ->  Seq Scan on k  (cost=0.00..1.10 rows=10 width=10)",),
            ],
        ),
    ]


@pytest.mark.backlog_fixed("RP-435")
@pytest.mark.parametrize(
    ("sql", "width"),
    [
        ("explain select 'x' from k, e where k.a = e.a", 32),
        ("explain select 'x', 1 from k join e on k.a = e.a", 36),
    ],
)
def test_explain_join_of_a_constant_select_list(rust_pg: Any, sql: str, width: int) -> None:
    """A constant in a join's select list adds its type's width to the
    join, on a comma join and an ON join alike, as PostgreSQL 15.19 prints
    them; each scan reads only its key."""
    got = _answers(rust_pg, _ANALYSED + [sql])[len(_ANALYSED) :]
    assert got == [
        (
            "ok",
            [
                (f"Hash Join  (cost=1.23..2.36 rows=10 width={width})",),
                ("  Hash Cond: (e.a = k.a)",),
                ("  ->  Seq Scan on e  (cost=0.00..1.10 rows=10 width=4)",),
                ("  ->  Hash  (cost=1.10..1.10 rows=10 width=4)",),
                ("        ->  Seq Scan on k  (cost=0.00..1.10 rows=10 width=4)",),
            ],
        ),
    ]


@pytest.mark.backlog("RP-1019")
def test_explain_prints_the_join(rust_pg: Any) -> None:
    """EXPLAIN of a DELETE ... USING a table prints the hash join
    PostgreSQL makes of it over two tables never analysed: priced from
    their default sizes (1,270 rows each), the hashed side is `k`. Here
    the join is unpriced and its sides stay in written order, as for the
    SELECT `select * from k, e where k.a = e.a`."""
    assert _answers(
        rust_pg,
        ["explain (costs off) delete from k using e where k.a = e.a"],
    ) == [
        (
            "ok",
            [
                ("Delete on k",),
                ("  ->  Hash Join",),
                ("        Hash Cond: (e.a = k.a)",),
                ("        ->  Seq Scan on e",),
                ("        ->  Hash",),
                ("              ->  Seq Scan on k",),
            ],
        ),
    ]


@pytest.mark.backlog_fixed("RP-436")
def test_missing_column_reported_before_ambiguous_one(rust_pg: Any) -> None:
    """PostgreSQL's `transformUpdateStmt` analyses the WHERE before the SET
    list, each left to right, and reports the first bad column it meets: a
    missing one there was 42702 here when a later name was ambiguous."""
    assert _answers(
        rust_pg,
        [
            "update k set a = a from (select 1 as a) t where t.nosuch = 1",
            "update k set b = b from e where e.nosuch = 1",
            "update k set b = 'x' from e where nosuch = 1 and b = 'y'",
            "delete from k using (select 1 as b) t where b = 'x' and t.nosuch = 1",
        ],
    ) == [
        ("err", "42703", "column t.nosuch does not exist"),
        ("err", "42703", "column e.nosuch does not exist"),
        ("err", "42703", 'column "nosuch" does not exist'),
        ("err", "42702", 'column reference "b" is ambiguous'),
    ]
