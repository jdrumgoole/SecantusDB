"""What working RP-754 (a window `min` / `max` over a network value picks by
`network_cmp`) found.

The type-order rewrite (`enum_order.rs`) reads the select list, the WHERE,
the HAVING and a window's ORDER BY as expressions, but read an ORDER BY key
only as a key: a comparison inside one compared the stored text (RP-762,
fixed). A window call in one is refused, whether written there or copied
from the select list by a positional `ORDER BY 1`. A windowed `min` / `max`
under a collation picked by the stored text's order (RP-763, fixed).

Measured on PostgreSQL 15.19 (the shared reference on port 5415): every
statement but the enum ones is VALUES-only and runs there as written; the
enum ones, which need a `CREATE TYPE`, were measured on a private 15.19
cluster (RP-763's after `_ENUM_SETUP`).
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


_NETS = "(values ('10.0.0.1'::inet), ('9.0.0.0'::inet), (null::inet)) s(x)"
_FALSE_FIRST = [("9.0.0.0/32",), ("10.0.0.1/32",), (None,)]

ORDER_BY_COMPARISON = {
    # `10.0.0.1 > 9.5.0.0` by `network_cmp`, though not by its text.
    "select x::text from (values ('10.0.0.1'::inet), ('9.0.0.0'::inet)) s(x)"
    " order by x > '9.5.0.0'::inet": [("9.0.0.0/32",), ("10.0.0.1/32",)],
    f"select x::text from {_NETS} order by not (x < '9.5.0.0'::inet), x::text": _FALSE_FIRST,
    # A positional key, or an output name, copied from the select list.
    f"select x::text, x > '9.5.0.0'::inet from {_NETS} order by 2, 1": [
        ("9.0.0.0/32", False),
        ("10.0.0.1/32", True),
        (None, None),
    ],
    f"select x::text, x > '9.5.0.0'::inet as g from {_NETS} order by g, 1": [
        ("9.0.0.0/32", False),
        ("10.0.0.1/32", True),
        (None, None),
    ],
    # A window's ORDER BY, and an aggregate's.
    f"select x::text, row_number() over (order by x > '9.5.0.0'::inet, x::text)"
    f" from {_NETS} order by 2": [("9.0.0.0/32", 1), ("10.0.0.1/32", 2), (None, 3)],
    f"select string_agg(x::text, ',' order by x > '9.5.0.0'::inet, x::text) from {_NETS}": [
        ("9.0.0.0/32,10.0.0.1/32",)
    ],
    f"select x::text from {_NETS} order by greatest(x, '9.5.0.0'::inet), x::text": [
        ("9.0.0.0/32",),
        (None,),
        ("10.0.0.1/32",),
    ],
    # A comparison under a locale collation compares its sort keys.
    "select x from (values ('a'), ('B'), ('c')) s(x) order by x collate \"en-x-icu\" < 'b', x": [
        ("B",),
        ("c",),
        ("a",),
    ],
    "select x from (values ('a'), ('B'), ('c')) s(x) order by x < 'b' collate \"en-x-icu\", x": [
        ("B",),
        ("c",),
        ("a",),
    ],
}


@pytest.mark.backlog_fixed("RP-762")
@pytest.mark.parametrize("query", list(ORDER_BY_COMPARISON))
def test_an_order_by_comparison_over_a_network_value(query: str, rust_pg: object) -> None:
    """A comparison over `inet` in an ORDER BY key compares by
    `network_cmp` on PostgreSQL 15.19, as the same comparison in the select
    list did here. Here it compared the values' text, so `10.0.0.1` was not
    greater than `9.5.0.0` and sorted with the false rows; a comparison
    under a collation compared the bytes. A sort key the type-order rewrite
    did not key was never rewritten as an expression."""
    assert _rows(rust_pg, query) == ORDER_BY_COMPARISON[query]


@pytest.mark.backlog_fixed("RP-762")
def test_an_order_by_comparison_over_an_enum_column(rust_pg: object) -> None:
    """A comparison over an enum table column in an ORDER BY key compares
    by declaration order. Measured on a private PostgreSQL 15.19 cluster
    (it needs a `CREATE TYPE`): `z, a, m`, as `z` is declared before `a`.
    Here it compared the labels' text, `a, z, m`."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("create type e762 as enum ('z', 'a', 'm')")
        c.execute("create table t762 (x e762)")
        c.execute("insert into t762 values ('a'), ('z'), ('m'), (null)")
    query = "select x::text as v from t762 order by x > 'a'::e762, x"
    assert _rows(rust_pg, query) == [("z",), ("a",), ("m",), (None,)]


WINDOWED_COLLATED = {
    'select min(x collate "en-x-icu") over (), max(x collate "en-x-icu") over ()'
    " from (values ('a'), ('B')) s(x)": [("a", "B")] * 2,
}


@pytest.mark.backlog_fixed("RP-763")
@pytest.mark.parametrize("query", list(WINDOWED_COLLATED))
def test_a_window_min_max_under_a_collation(query: str, rust_pg: object) -> None:
    """A windowed `min` / `max` under the `en-x-icu` collation picks `a` and
    `B` on PostgreSQL 15.19, as the plain aggregate does. The window call
    picked by the stored text's byte order, `B` and `a`, until RP-763."""
    assert _rows(rust_pg, query) == WINDOWED_COLLATED[query]


_ENUM_SETUP = [
    "drop table if exists t763",
    "drop type if exists e763",
    "create type e763 as enum ('zeta', 'alpha', 'mid')",
    "create table t763 (k int, e e763)",
    "insert into t763 values (1, 'alpha'), (2, 'zeta'), (3, 'mid'), (4, null)",
]

WINDOWED_ENUM = {
    # By label position, `zeta` < `alpha` < `mid`; by text `alpha` is least.
    "select k, min(e) over (), max(e) over (order by k), pg_typeof(max(e) over ())::text"
    " from t763 order by k": [
        (1, "zeta", "alpha", "e763"),
        (2, "zeta", "alpha", "e763"),
        (3, "zeta", "mid", "e763"),
        (4, "zeta", "mid", "e763"),
    ],
    "select k, min(e) over (order by k rows between 1 preceding and current row)"
    " from t763 order by k": [(1, "alpha"), (2, "zeta"), (3, "zeta"), (4, "mid")],
}


@pytest.mark.backlog_fixed("RP-763")
@pytest.mark.parametrize("query", list(WINDOWED_ENUM))
def test_a_window_min_max_over_an_enum(query: str, rust_pg: object) -> None:
    """A windowed `min` / `max` over an enum picks by the labels' sort order
    on PostgreSQL 15.19, as the plain aggregate does, and is of the enum
    type. The window call picked by the labels' text until RP-763."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _ENUM_SETUP:
            c.execute(q)
    try:
        assert _rows(rust_pg, query) == WINDOWED_ENUM[query]
    finally:
        with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
            for q in _ENUM_SETUP[:2]:
                c.execute(q)
