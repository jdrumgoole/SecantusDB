"""What working RP-761 (a window call in an ORDER BY key) found.

EXPLAIN printed a Sort's key by the synthetic field the planner gave it, not
by the expression PostgreSQL deparses: a computed key was `__order0`, and a
key that is a window target the target's output name (`sum`). Fixed by
RP-840.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t761 cascade",
    "create table t761 (x int)",
    "insert into t761 values (1), (2), (3)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _plan(dsn: str, query: str) -> list[str]:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        return [r[0] for r in c.execute(query).fetchall()]


SORT_KEYS = {
    "explain (costs off) select x from t761 order by x + 1": [
        "Sort",
        "  Sort Key: ((x + 1))",
        "  ->  Seq Scan on t761",
    ],
    "explain (costs off) select x, sum(x) over (order by x) from t761"
    " order by sum(x) over (order by x) desc": [
        "Sort",
        "  Sort Key: (sum(x) OVER (?)) DESC",
        "  ->  WindowAgg",
        "        ->  Sort",
        "              Sort Key: x",
        "              ->  Seq Scan on t761",
    ],
    "explain (costs off) select x, sum(x) over (order by x) from t761"
    " order by sum(x) over (order by x) + 1 desc": [
        "Sort",
        "  Sort Key: ((sum(x) OVER (?) + 1)) DESC",
        "  ->  WindowAgg",
        "        ->  Sort",
        "              Sort Key: x",
        "              ->  Seq Scan on t761",
    ],
}


@pytest.mark.backlog_fixed("RP-840")
@pytest.mark.parametrize("query", list(SORT_KEYS))
def test_explain_prints_a_computed_sort_key_as_its_expression(dsn: str, query: str) -> None:
    """PostgreSQL 15.19 deparses a Sort's key (`((x + 1))`, `(sum(x) OVER
    (?))`). A computed key printed as its synthetic field, `__order0`, and
    a window target's key as the target's name, `sum`, until RP-840."""
    assert _plan(dsn, query) == SORT_KEYS[query]
