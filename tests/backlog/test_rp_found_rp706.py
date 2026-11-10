"""What working RP-706 (`min` / `max` over a `reg*` or `cidr` value coerce
their input to the overload's type) found.

A window `min` / `max` over a network value compares the values' TEXT: the
plain aggregate is rewritten to compare `network_cmp`'s order
(`__net_sortkey`, RP-610), the windowed call is not.

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement is VALUES-only and runs there as written.
"""

from __future__ import annotations

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


WINDOWED = {
    # IPv4 before IPv6, then by address: `::1` is the largest, `9.0.0.0` the
    # smallest. By text, `10.0.0.1` sorts first.
    "select (max(x) over ())::text, (min(x) over ())::text"
    " from (values ('10.0.0.1'::inet), ('9.0.0.0'::inet), ('::1'::inet)) s(x)": [
        ("::1/128", "9.0.0.0/32")
    ]
    * 3,
    # `max(cidr)` is `max(inet)`, by the same order.
    "select (max(x) over ())::text, pg_typeof(max(x) over ())::text"
    " from (values ('10.0.0.1/32'::cidr), ('9.0.0.0/8'::cidr)) s(x)": [("10.0.0.1/32", "inet")] * 2,
    # A running max over an ORDER BY frame.
    "select (max(x) over (order by x))::text"
    " from (values ('10.0.0.1'::inet), ('9.0.0.0'::inet)) s(x) order by 1": [
        ("10.0.0.1/32",),
        ("9.0.0.0/32",),
    ],
}


@pytest.mark.backlog("RP-754")
@pytest.mark.parametrize("query", list(WINDOWED))
def test_a_window_min_max_over_a_network_value_uses_network_order(
    query: str, rust_pg: object
) -> None:
    """A windowed `min` / `max` over `inet` or `cidr` picks by
    `network_cmp` on PostgreSQL 15.19, as the plain aggregate does. Here it
    picks by the values' text, and over an ORDER BY frame it is 0A000
    `function max(inet) is not supported yet`."""
    assert _rows(rust_pg, query) == WINDOWED[query]
