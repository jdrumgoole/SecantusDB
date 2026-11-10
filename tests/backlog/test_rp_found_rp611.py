"""Gap tests of the divergences found while working RP-611 (an array of
`oidvector`s orders its elements as int arrays): a VALUES list's network
column ordered by its text (RP-651, fixed by RP-610's typing of a derived
column: its test is now a regression test), and an ORDER BY naming a window
function whose window is ordered by a keyed type was refused (RP-652, fixed
by RP-754: a positional key naming a window target is no longer copied into
the sort clause).

The expected answers were measured on PostgreSQL 15.19; none needs DDL.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.conftest import pg_answer
from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


# `network_cmp` orders 9.0.0.1 before 10.0.0.1; text order puts it after.
VALUES_NETWORK_ORDER: dict[str, list[tuple[object, ...]]] = {
    "select v::text t from (values ('10.0.0.1'::inet), ('9.0.0.1')) s(v) order by v": [
        ("9.0.0.1/32",),
        ("10.0.0.1/32",),
    ],
    "select v::text t, row_number() over (order by v) "
    "from (values ('10.0.0.1'::inet), ('9.0.0.1')) s(v) order by t": [
        ("10.0.0.1/32", 2),
        ("9.0.0.1/32", 1),
    ],
}


@pytest.mark.backlog_fixed("RP-651")
@pytest.mark.parametrize("query", list(VALUES_NETWORK_ORDER))
def test_values_network_column_orders_by_network_cmp(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("ok", VALUES_NETWORK_ORDER[query])


# The outer ORDER BY names the window function's column, by position or by
# its alias, while the window orders by a range or an `oidvector`.
WINDOW_TARGET_ORDER: dict[str, list[tuple[object, ...]]] = {
    "select v::text t, row_number() over (order by v) "
    "from (values ('[1,2]'::int4range), ('[0,5]')) s(v) order by 2": [
        ("[0,6)", 1),
        ("[1,3)", 2),
    ],
    "select v::text t, row_number() over (order by v) r "
    "from (values ('[1,2]'::int4range), ('[0,5]')) s(v) order by r": [
        ("[0,6)", 1),
        ("[1,3)", 2),
    ],
    "select v::text t, row_number() over (order by v) "
    "from (values ('1 2'::oidvector), ('3')) s(v) order by 2": [
        ("3", 1),
        ("1 2", 2),
    ],
}


@pytest.mark.backlog_fixed("RP-652")
@pytest.mark.parametrize("query", list(WINDOW_TARGET_ORDER))
def test_order_by_a_window_target_over_a_keyed_window(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("ok", WINDOW_TARGET_ORDER[query])
