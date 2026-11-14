"""What working RP-703 (a WHERE over `generate_series` that does not lower
to a filter runs per row) found:

- EXPLAIN of a `generate_series` in FROM printed neither its alias nor its
  WHERE as a `Filter:` line (fixed by RP-847); with a WHERE, its estimate
  was `rows=0 width=0` until RP-847 priced it.

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is a FROM function and runs there as written.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def plan(dsn: str, query: str) -> list[str]:
    with psycopg.connect(dsn, autocommit=True) as c:
        return [r[0] for r in c.execute(query).fetchall()]


# `ExplainNode` names a Function Scan's alias when it differs from the
# function's name, and `show_scan_qual` prints the quals the scan applies to
# each row. Here each prints the bare `Function Scan on generate_series`.
SERIES_SCAN_EXPLAIN = {
    "explain (costs off) select 1 from generate_series(1, 2) f(x) where x > 1": [
        "Function Scan on generate_series f",
        "  Filter: (x > 1)",
    ],
    "explain (costs off) select 1 from generate_series(1, 2) f(x) where f.x + 1 > 0": [
        "Function Scan on generate_series f",
        "  Filter: ((x + 1) > 0)",
    ],
    "explain (costs off) select 1 from generate_series(1, 2) f(x)": [
        "Function Scan on generate_series f",
    ],
    "explain (costs off) select 1 from generate_series(1, 2) where generate_series > 1": [
        "Function Scan on generate_series",
        "  Filter: (generate_series > 1)",
    ],
}


@pytest.mark.backlog_fixed("RP-847")
@pytest.mark.parametrize("query", list(SERIES_SCAN_EXPLAIN))
def test_a_series_scan_explains_its_alias_and_filter(query: str, dsn: str) -> None:
    assert plan(dsn, query) == SERIES_SCAN_EXPLAIN[query]


# `cost_functionscan` prices the function's rows (`prorows`, or the support
# function's count) and the Filter's quals per row; `clauselist_selectivity`
# gives the default 1/3 for an inequality over a function's output. Here a
# filtered series scan's estimate was `rows=0 width=0` until RP-847.
SERIES_SCAN_COSTS = {
    "explain select 1 from generate_series(1, 2) f(x) where x > 1": [
        "Function Scan on generate_series f  (cost=0.00..0.03 rows=1 width=4)",
        "  Filter: (x > 1)",
    ],
    "explain select 1 from generate_series(1, 10) f(x) where x + 1 > 0": [
        "Function Scan on generate_series f  (cost=0.00..0.15 rows=3 width=4)",
        "  Filter: ((x + 1) > 0)",
    ],
    "explain select 1 from generate_series(1, 10) f(x) where x = 3": [
        "Function Scan on generate_series f  (cost=0.00..0.13 rows=1 width=4)",
        "  Filter: (x = 3)",
    ],
    "explain select 1 from generate_series(1, 1000) f(x) where x = 3": [
        "Function Scan on generate_series f  (cost=0.00..12.50 rows=5 width=4)",
        "  Filter: (x = 3)",
    ],
    "explain select 1 from generate_series(1, 10) f(x) where x <> 3": [
        "Function Scan on generate_series f  (cost=0.00..0.13 rows=9 width=4)",
        "  Filter: (x <> 3)",
    ],
    "explain select 1 from generate_series(1, 1000) f(x) where x <> 3": [
        "Function Scan on generate_series f  (cost=0.00..12.50 rows=995 width=4)",
        "  Filter: (x <> 3)",
    ],
    "explain select 1 from generate_series(1, 10) f(x) where x > 1 and x < 5": [
        "Function Scan on generate_series f  (cost=0.00..0.15 rows=1 width=4)",
        "  Filter: ((x > 1) AND (x < 5))",
    ],
    "explain select 1 from generate_series(1, 1000) f(x) where x > 1 and x < 5": [
        "Function Scan on generate_series f  (cost=0.00..15.00 rows=5 width=4)",
        "  Filter: ((x > 1) AND (x < 5))",
    ],
    "explain select 1 from generate_series(1, 10) f(x) where x * 2 = 4": [
        "Function Scan on generate_series f  (cost=0.00..0.15 rows=1 width=4)",
        "  Filter: ((x * 2) = 4)",
    ],
    "explain select 1 from generate_series(1, 1000) f(x) where x * 2 = 4": [
        "Function Scan on generate_series f  (cost=0.00..15.00 rows=5 width=4)",
        "  Filter: ((x * 2) = 4)",
    ],
    "explain select 1 from generate_series(1, 10) f(x) where x > 1 or x < 0": [
        "Function Scan on generate_series f  (cost=0.00..0.15 rows=6 width=4)",
        "  Filter: ((x > 1) OR (x < 0))",
    ],
    "explain select 1 from generate_series(1, 1000) f(x) where x > 1 or x < 0": [
        "Function Scan on generate_series f  (cost=0.00..15.00 rows=556 width=4)",
        "  Filter: ((x > 1) OR (x < 0))",
    ],
    "explain select 1 from generate_series(1, 10) f(x) where 3 = x": [
        "Function Scan on generate_series f  (cost=0.00..0.13 rows=1 width=4)",
        "  Filter: (3 = x)",
    ],
    "explain select 1 from generate_series(1, 1000) f(x) where 3 = x": [
        "Function Scan on generate_series f  (cost=0.00..12.50 rows=5 width=4)",
        "  Filter: (3 = x)",
    ],
}


@pytest.mark.backlog_fixed("RP-847")
@pytest.mark.parametrize("query", list(SERIES_SCAN_COSTS))
def test_a_filtered_series_scan_is_priced(query: str, dsn: str) -> None:
    assert plan(dsn, query) == SERIES_SCAN_COSTS[query]
