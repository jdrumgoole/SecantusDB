"""EXPLAIN (FORMAT JSON) of a join on the Rust PG server (RP-353).

PostgreSQL 15.19 gives every join node a `Join Type` and an `Inner Unique`
(`ExplainNode`), measured on a freshly initdb'd cluster: for
`select * from e, e e2 where e.a < e2.a` the Nested Loop's are `Inner` and
`false`.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn
from tests.backlog.test_rp_explain_costs import _ANALYSED, fresh_db


@pytest.mark.backlog("RP-353")
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
