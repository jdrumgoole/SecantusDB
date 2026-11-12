"""What working RP-682 (OVER on a plain function is 42809) found:

- RP-868 (fixed): an aggregate-only clause -- DISTINCT, ORDER BY, FILTER -- on
  a plain function call without OVER was accepted and ignored, where
  PostgreSQL refuses it 42809;
- RP-869: an ordered-set aggregate's OVER and WITHIN GROUP checks: `rank(1) over
  ()` answers 1, and `percentile_cont(...) within group (...) over ()` is
  this server's gap message, not PostgreSQL's;
- RP-870 (fixed): the aggregates PostgreSQL runs over a window that this
  server refused 0A000 (`stddev`, `variance`, `bit_and`, `every`, `jsonb_agg`,
  `corr`, `xmlagg`, ...).

Measured on PostgreSQL 15.19 (the shared reference on port 5415), VALUES-only
statements as written.
"""

from __future__ import annotations

from decimal import Decimal

import psycopg
import pytest
from psycopg.types.multirange import Multirange
from psycopg.types.range import Range

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> str:
    return _dsn(rust_pg) + " dbname=postgres"


def _rows(dsn: str, query: str) -> object:
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


PLAIN_CLAUSES = {
    "select upper(distinct 'a')": "DISTINCT",
    "select upper('a' order by 1)": "ORDER BY",
    "select upper('a') filter (where true)": "FILTER",
}


@pytest.mark.backlog_fixed("RP-868")
@pytest.mark.parametrize("query", list(PLAIN_CLAUSES))
def test_an_aggregate_clause_on_a_plain_function_is_42809(query: str, dsn: str) -> None:
    clause = PLAIN_CLAUSES[query]
    assert _rows(dsn, query) == (
        "error",
        "42809",
        f"{clause} specified, but upper is not an aggregate function",
    )


ORDERED_SET = {
    "select rank(1) over ()": (
        "error",
        "42809",
        "WITHIN GROUP is required for ordered-set aggregate rank",
    ),
    "select percentile_cont(0.5) within group (order by v) over () from (values (1)) s(v)": (
        "error",
        "0A000",
        "OVER is not supported for ordered-set aggregate percentile_cont",
    ),
}


@pytest.mark.backlog("RP-869")
@pytest.mark.parametrize("query", list(ORDERED_SET))
def test_an_ordered_set_aggregate_over_a_window_is_refused(query: str, dsn: str) -> None:
    assert _rows(dsn, query) == ORDERED_SET[query]


# Three rows of an int and a float8 column.
_V = "from (values (1, 2.0::float8), (3, 4.0), (5, 7.0)) s(v, f)"

WINDOWED_AGGREGATES = {
    "select v, stddev(v) over (order by v) from (values (1), (3), (5)) s(v)": [
        (1, None),
        (3, Decimal("1.4142135623730950")),
        (5, Decimal("2.0000000000000000")),
    ],
    "select v, variance(v) over (order by v) from (values (1), (3), (5)) s(v)": [
        (1, None),
        (3, Decimal("2.0000000000000000")),
        (5, Decimal("4.0000000000000000")),
    ],
    "select v, bit_and(v) over (order by v) from (values (7), (3), (5)) s(v)": [
        (3, 3),
        (5, 1),
        (7, 1),
    ],
    "select v, every(v > 2) over (order by v) from (values (1), (3), (5)) s(v)": [
        (1, False),
        (3, False),
        (5, False),
    ],
    "select v, jsonb_agg(v) over (order by v) from (values (1), (3), (5)) s(v)": [
        (1, [1]),
        (3, [1, 3]),
        (5, [1, 3, 5]),
    ],
    # Two per-row arguments, and an empty frame answering the aggregate over
    # no rows (`regr_count` 0, not NULL).
    f"select v, corr(f, v) over (order by v) {_V}": [
        (1, None),
        (3, 1.0),
        (5, 0.9933992677987828),
    ],
    f"select v, regr_count(f, v) over (order by v rows between 1 following and 1 following) {_V}": [
        (1, 1),
        (3, 1),
        (5, 0),
    ],
    f"select v, jsonb_object_agg(v, f) over (order by v) {_V}": [
        (1, {"1": 2}),
        (3, {"1": 2, "3": 4}),
        (5, {"1": 2, "3": 4, "5": 7}),
    ],
    f"select v, xmlagg(xmlelement(name a, v)) over (order by v) {_V}": [
        (1, "<a>1</a>"),
        (3, "<a>1</a><a>3</a>"),
        (5, "<a>1</a><a>3</a><a>5</a>"),
    ],
    f"select v, range_agg(int4range(v, v + 1)) over (order by v) {_V}": [
        (1, Multirange([Range(1, 2, "[)")])),
        (3, Multirange([Range(1, 2, "[)"), Range(3, 4, "[)")])),
        (5, Multirange([Range(1, 2, "[)"), Range(3, 4, "[)"), Range(5, 6, "[)")])),
    ],
    f"select v, var_pop(v) filter (where v > 1) over (order by v) {_V}": [
        (1, None),
        (3, Decimal("0")),
        (5, Decimal("1.00000000000000000000")),
    ],
    # The checks the call without OVER makes, and the window's own refusal.
    f"select v, bit_and(f) over () {_V}": (
        "error",
        "42883",
        "function bit_and(double precision) does not exist",
    ),
    f"select v, array_agg(v order by v) over () {_V}": (
        "error",
        "0A000",
        "aggregate ORDER BY is not implemented for window functions",
    ),
}


@pytest.mark.backlog_fixed("RP-870")
@pytest.mark.parametrize("query", list(WINDOWED_AGGREGATES))
def test_more_aggregates_run_over_a_window(query: str, dsn: str) -> None:
    """Every plain aggregate ran over a window, as the same call without
    OVER computes it over a group."""
    assert _rows(dsn, query) == WINDOWED_AGGREGATES[query]
