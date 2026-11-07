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


@pytest.mark.backlog("RP-435")
def test_explain_prints_the_join_and_the_index_scan(rust_pg: Any) -> None:
    """EXPLAIN of an UPDATE ... FROM or a DELETE ... USING prints the plan
    PostgreSQL makes for the join: a FROM-subquery of one row pulled up into
    an Index Cond on the target's key, and a table in USING hash-joined."""
    assert _answers(
        rust_pg,
        [
            "explain (costs off) delete from k using (select 50 as a) t where k.a = t.a",
            "explain (costs off) update k set b = 'x' from (select 50 as a) t where k.a = t.a",
            "explain (costs off) delete from k using e where k.a = e.a",
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


@pytest.mark.backlog("RP-436")
def test_missing_column_reported_before_ambiguous_one(rust_pg: Any) -> None:
    """PostgreSQL's `transformUpdateStmt` analyses the WHERE before the SET
    list, each left to right, and reports the first bad column it meets: a
    missing one there is 42703 even when a later name is ambiguous."""
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
