"""EXPLAIN (FORMAT JSON) of a join on the Rust PG server (RP-353, fixed).

PostgreSQL 15.19 gives every join node a `Join Type` and an `Inner Unique`
(`ExplainNode`), measured on a freshly initdb'd cluster: for
`select * from e, e e2 where e.a < e2.a` the Nested Loop's are `Inner` and
`false`. The server printed neither; it now does, `Inner Unique` decided as
`innerrel_is_unique` decides it for a base relation.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_explain_costs import _ANALYSED, fresh_db


@pytest.mark.backlog_fixed("RP-353")
def test_a_join_node_names_its_join_type(rust_pg: Any) -> None:
    with psycopg.connect(fresh_db(_dsn(rust_pg))) as c:
        try:
            for s in _ANALYSED:
                c.execute(s)
            plan = c.execute(
                "explain (format json) select * from e, e e2 where e.a < e2.a"
            ).fetchone()[0][0]["Plan"]
        finally:
            c.rollback()
    keys = [k for k in plan if k != "Plans"]
    assert keys == [
        "Node Type",
        "Parallel Aware",
        "Async Capable",
        "Join Type",
        "Startup Cost",
        "Total Cost",
        "Plan Rows",
        "Plan Width",
        "Inner Unique",
        "Join Filter",
    ]
    assert (plan["Join Type"], plan["Inner Unique"]) == ("Inner", False)


# (join, Join Type, Inner Unique) of the top join node, PostgreSQL 15.19
# (measured 2026-11-04 on port 5415): a primary key or a unique index whose
# every column the join clause or a constant fixes is unique; a DEFERRABLE
# one, or one only partly fixed, is not.
_UNIQUE_CASES = [
    ("select * from r join p on r.x = p.id", "Inner", True),
    ("select * from r left join p on r.x = p.id", "Left", True),
    ("select * from r full join p on r.x = p.id", "Full", True),
    ("select * from r join q on r.x = q.a", "Inner", False),
    ("select * from r join q on r.x = q.a where q.b = 3", "Inner", True),
    ("select * from r join d on r.x = d.id", "Inner", False),
]


@pytest.mark.backlog_fixed("RP-353")
@pytest.mark.parametrize(("sql", "join_type", "unique"), _UNIQUE_CASES)
def test_inner_unique_follows_the_inner_tables_unique_index(
    rust_pg: Any, sql: str, join_type: str, unique: bool
) -> None:
    with psycopg.connect(fresh_db(_dsn(rust_pg))) as c:
        try:
            for s in (
                "create table p (id int primary key, v int)",
                "create table q (a int, b int, unique (a, b))",
                "create table r (x int)",
                "create table d (id int unique deferrable)",
            ):
                c.execute(s)
            plan = c.execute(f"explain (format json, costs off) {sql}").fetchone()[0][0]["Plan"]
        finally:
            c.rollback()
    # The node type is the cost model's choice (PostgreSQL merge-joins the
    # third case); the join's type and uniqueness are not.
    assert (plan["Join Type"], plan["Inner Unique"]) == (join_type, unique)


@pytest.mark.backlog("RP-355")
def test_a_distinct_subquery_inner_side_is_unique(rust_pg: Any) -> None:
    """PostgreSQL 15.19 proves a DISTINCT subquery's output unique on its
    columns (`rel_is_distinct_for`): `r join (select distinct x from r) s on
    r.x = s.x` is a Hash Join with `Inner Unique: true`."""
    with psycopg.connect(fresh_db(_dsn(rust_pg))) as c:
        try:
            c.execute("create table r (x int)")
            plan = c.execute(
                "explain (format json, costs off) select * from r"
                " join (select distinct x from r) s on r.x = s.x"
            ).fetchone()[0][0]["Plan"]
        finally:
            c.rollback()
    assert (plan["Join Type"], plan["Inner Unique"]) == ("Inner", True)
