"""What working RP-754 (a window `min` / `max` over a network value picks by
`network_cmp`) found.

The type-order rewrite (`enum_order.rs`) reads the select list, the WHERE,
the HAVING and a window's ORDER BY as expressions, but an ORDER BY key only
as a key: a comparison inside one is left to compare the stored text, and a
window call in one is refused, whether written there or copied from the
select list by a positional `ORDER BY 1`. A windowed `min` / `max` under a
collation still picks by the stored text's order.

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
"""

from __future__ import annotations

from ipaddress import IPv4Address

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def _rows(rust_pg: object, query: str) -> object:
    """The rows, or the error's SQLSTATE and message."""
    try:
        with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
            return [tuple(r) for r in c.execute(query).fetchall()]
    except psycopg.Error as e:
        return (e.sqlstate, str(e).splitlines()[0])


WINDOW_IN_ORDER_BY = {
    # A window call written in the ORDER BY: the sort is over its value.
    "select max(x::text) over (order by x) from (values (1), (2)) s(x)"
    " order by max(x::text) over (order by x) desc": [("2",), ("1",)],
    "select sum(x) over (order by x) + 1 from (values (1), (2)) s(x)"
    " order by sum(x) over (order by x) desc": [(4,), (2,)],
    # A positional key naming a window target of a network type.
    "select max(x) over (order by x)"
    " from (values ('10.0.0.1'::inet), ('9.0.0.0'::inet)) s(x) order by 1 desc": [
        (IPv4Address("10.0.0.1"),),
        (IPv4Address("9.0.0.0"),),
    ],
}


@pytest.mark.backlog("RP-761")
@pytest.mark.parametrize("query", list(WINDOW_IN_ORDER_BY))
def test_a_window_call_in_an_order_by_key(query: str, rust_pg: object) -> None:
    """An ORDER BY key holding a window call sorts by the call's value on
    PostgreSQL 15.19. Here it is 0A000 `function max(text) is not supported
    yet` (`sum(integer)`): the key is evaluated as a scalar call."""
    assert _rows(rust_pg, query) == WINDOW_IN_ORDER_BY[query]


ORDER_BY_COMPARISON = {
    # `10.0.0.1 > 9.5.0.0` by `network_cmp`, though not by its text.
    "select x::text from (values ('10.0.0.1'::inet), ('9.0.0.0'::inet)) s(x)"
    " order by x > '9.5.0.0'::inet": [("9.0.0.0/32",), ("10.0.0.1/32",)],
}


@pytest.mark.backlog("RP-762")
@pytest.mark.parametrize("query", list(ORDER_BY_COMPARISON))
def test_an_order_by_comparison_over_a_network_value(query: str, rust_pg: object) -> None:
    """A comparison over `inet` in an ORDER BY key compares by
    `network_cmp` on PostgreSQL 15.19, as the same comparison in the select
    list does here. Here it compares the values' text, so `10.0.0.1` is not
    greater than `9.5.0.0` and sorts with the false rows."""
    assert _rows(rust_pg, query) == ORDER_BY_COMPARISON[query]


WINDOWED_COLLATED = {
    'select min(x collate "en-x-icu") over (), max(x collate "en-x-icu") over ()'
    " from (values ('a'), ('B')) s(x)": [("a", "B")] * 2,
}


@pytest.mark.backlog("RP-763")
@pytest.mark.parametrize("query", list(WINDOWED_COLLATED))
def test_a_window_min_max_under_a_collation(query: str, rust_pg: object) -> None:
    """A windowed `min` / `max` under the `en-x-icu` collation picks `a` and
    `B` on PostgreSQL 15.19, as the plain aggregate does here. Here the
    window call picks by the stored text's byte order, `B` and `a`."""
    assert _rows(rust_pg, query) == WINDOWED_COLLATED[query]
